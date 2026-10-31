//! Cluster terrain: the material the terrain blocks of a cluster package are drawn with, the
//! package's terrain metadata, and the clip that hides the cluster terrain where the stock terrain
//! already draws a quadrant in full detail.
//!
//! The converter publishes terrain as blocks of up to 16x16 cells, each a cluster mesh with one
//! baked albedo atlas. This module reads three tables of the overlay database (`clusters.db`,
//! attached read-only to the base dataset's `skyrim_world.db`):
//!
//! * `cluster_terrain(mesh_hash, primitive_index, worldspace_id, anchor_x, anchor_y, side_cells,
//!   instance_path, atlas_path, source_cells)`, one row per block. `anchor_x` / `anchor_y` are the
//!   **absolute** cell grid of the block's south-west cell (Creation grid, north is +y), not the
//!   cluster header's LOD-relative anchor. `source_cells` is a JSON array `[[grid_x, grid_y], ...]`
//!   of the cells the block was built from, all inside the `side_cells` square at the anchor.
//! * `cluster_terrain_heights(worldspace_id, grid_x, grid_y, heights)`, the canonical 33x33 height
//!   grid (`f32` little endian) of every source cell. The stock near terrain takes its heights from
//!   here, so both draw the same surface.
//! * `cluster_terrain_build(version, cell_cache_hash)`, which converted dataset's cell cache the
//!   package was built from. Loading certified terrain compares this with the launch envelope
//!   receipt; only the certification I/O worker hashes the dataset cache.
//!
//! Nothing is guessed: a missing table, a count that does not match, a non-finite height, a source
//! cell outside its block or claimed by two blocks, a path that leaves the package, or a cell with
//! no heights (or heights for no cell) refuses the whole terrain package with the reason.
//!
//! # The clip
//!
//! The material is Bevy's standard material extended with [`TerrainClipExtension`]. The extension
//! carries, per block, the block's runtime origin and a bit per half-cell quadrant of the block:
//! set where the stock terrain draws that quadrant in full detail, which the shader then discards
//! in the forward, prepass and shadow passes alike (`shaders/terrain_clip.wgsl`). An empty mask
//! discards nothing; a block of another worldspace than the active one discards everything.
//!
//! The material is an alpha mask with a cutoff of 0 (every fragment passes the stock alpha test).
//! That is deliberate: Bevy compiles the fragment stage of the depth-only and shadow pipelines
//! only for materials that may discard, and an opaque material would skip it and keep writing the
//! clipped terrain into the shadow maps.

use crate::world::components::CELL_SIZE;
use bevy::{
    asset::RenderAssetUsages,
    image::{CompressedImageFormats, ImageSampler, ImageSamplerDescriptor, ImageType},
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::{
        extract_resource::ExtractResource,
        render_resource::{AsBindGroup, ShaderType, TextureDimension},
    },
    shader::ShaderRef,
};
#[cfg(test)]
use rusqlite::OpenFlags;
use rusqlite::{Connection, types::ValueRef};
#[cfg(test)]
use sha2::{Digest, Sha256};
use shared::cluster_mapping::HashedPayload;
#[cfg(test)]
use std::io::Read;
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

/// The terrain material: the standard material of the baked atlas, plus the near-stock clip.
pub type ClusterTerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainClipExtension>;

/// The package database, inside the package folder.
/// The converted dataset's cell cache, inside the dataset folder.
const CELL_CACHE_FILE: &str = "cell_cache.rkyv";
/// The `cluster_terrain_build.version` this module reads.
const TERRAIN_BUILD_VERSION: i64 = 1;
/// Most cells along a block's side: the clip mask is 32 rows of 32 quadrant bits.
pub const MAX_BLOCK_SIDE_CELLS: u32 = 16;
/// One view-local terrain clip record: origin, state and eight mask vectors.
pub const TERRAIN_CLIP_VEC4S: u32 = 10;
/// Forward and depth/shadow each choose their stock complement independently.
pub const TERRAIN_VIEW_VEC4S: u32 = TERRAIN_CLIP_VEC4S * 2;
/// Vertices along one side of a canonical height grid (32 intervals per cell).
const HEIGHT_GRID_SIDE: usize = 33;
/// The roughness the terrain LOD atlas material is published with.
use shared::terrain::ATLAS_ROUGHNESS;
/// Quadrant rows in the mask, and the `vec4<u32>` words that hold them.
const MASK_ROWS: usize = 32;
const MASK_WORDS: usize = MASK_ROWS / 4;

/// One terrain block of the package.
#[derive(Debug, Clone)]
pub struct TerrainBlock {
    /// The block's cluster mesh: the `cluster_meshes` key.
    pub mesh_hash: String,
    pub primitive_index: u32,
    pub worldspace_id: u32,
    /// The absolute cell grid of the block's south-west cell.
    pub anchor: IVec2,
    pub side_cells: u32,
    /// The block's instance payload, relative to the package.
    pub instance_path: String,
    /// The absolute cell grids the block was built from, all inside the block's square.
    pub cells: Vec<IVec2>,
    /// The block's own material: the atlas plus the block's clip settings.
    pub material: Handle<ClusterTerrainMaterial>,
    /// The cluster batch the block draws in, once the batch exists.
    pub batch: Option<u32>,
}

/// Immutable geometry needed to select terrain independently for each retained view.
/// Material and batch ownership stay with the package/render scene.
#[derive(Clone, Debug)]
pub struct TerrainViewBlock {
    pub worldspace_id: u32,
    /// Absolute cell grid of the block's south-west cell.
    pub anchor: IVec2,
    pub side_cells: u32,
    pub cells: Vec<IVec2>,
    /// Material batch resolved for THIS block, independent of pooled geometry.
    pub batch: Option<u32>,
}

impl From<&TerrainBlock> for TerrainViewBlock {
    fn from(block: &TerrainBlock) -> Self {
        Self {
            worldspace_id: block.worldspace_id,
            anchor: block.anchor,
            side_cells: block.side_cells,
            cells: block.cells.clone(),
            batch: block.batch,
        }
    }
}

impl TerrainViewBlock {
    /// Append exactly ten vec4 words in the shader's explicit little-endian layout.
    /// `covered` includes only stock quadrants proven drawable in this same view.
    pub fn append_clip_record(
        &self,
        world: Option<u32>,
        origin: IVec2,
        covered: &HashSet<(IVec2, u8)>,
        ready: bool,
        output: &mut Vec<u8>,
    ) {
        let settings = TerrainClipSettings::for_geometry(
            TerrainClipGeometry::from(self),
            world,
            origin,
            covered,
            ready,
        );
        for vector in [settings.origin_and_size, settings.state] {
            for word in vector.to_array() {
                output.extend_from_slice(&word.to_bits().to_le_bytes());
            }
        }
        for vector in settings.rows {
            for word in vector.to_array() {
                output.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
}

/// Shared borrowed geometry keeps legacy material settings and per-view records
/// on precisely the same coordinate/quadrant recipe without cloning cells per frame.
struct TerrainClipGeometry<'a> {
    worldspace_id: u32,
    anchor: IVec2,
    side_cells: u32,
    cells: &'a [IVec2],
}

impl<'a> From<&'a TerrainBlock> for TerrainClipGeometry<'a> {
    fn from(block: &'a TerrainBlock) -> Self {
        Self {
            worldspace_id: block.worldspace_id,
            anchor: block.anchor,
            side_cells: block.side_cells,
            cells: &block.cells,
        }
    }
}

impl<'a> From<&'a TerrainViewBlock> for TerrainClipGeometry<'a> {
    fn from(block: &'a TerrainViewBlock) -> Self {
        Self {
            worldspace_id: block.worldspace_id,
            anchor: block.anchor,
            side_cells: block.side_cells,
            cells: &block.cells,
        }
    }
}

/// The last inputs of [`ClusterTerrain::set_near_coverage`], so an unchanged call does nothing.
struct NearInputs {
    world: Option<u32>,
    origin: IVec2,
    covered: HashSet<(IVec2, u8)>,
    /// Per block: whether the renderer could draw it, which decides if it draws at all.
    ready: Vec<bool>,
}

/// What `cluster_terrain_build` says the package was built from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TerrainBuild {
    cell_cache_hash: [u8; 32],
}

/// Cheap immutable handle to the exact canonical heights loaded with terrain.
/// The handle alone grants no package certification; readers still compare its
/// selected bytes with the retained certificate's database.
#[derive(Clone)]
pub(crate) struct CanonicalTerrainHeights(Arc<HashMap<(u32, i32, i32), Vec<f32>>>);

impl CanonicalTerrainHeights {
    pub(crate) fn get(&self, world: u32, grid: IVec2) -> Option<&[f32]> {
        self.0.get(&(world, grid.x, grid.y)).map(Vec::as_slice)
    }

    // Compare source allocation identity without copying the height inventory.
    #[cfg(test)]
    pub(crate) fn same_allocation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// The terrain of a cluster package: its blocks and the canonical heights.
#[derive(Resource)]
pub struct ClusterTerrain {
    pub blocks: Vec<TerrainBlock>,
    worldspaces: HashSet<u32>,
    heights: CanonicalTerrainHeights,
    by_mesh: HashMap<String, Vec<(u32, usize)>>,
    build: TerrainBuild,
    near: Option<NearInputs>,
}

impl ClusterTerrain {
    /// Reads and validates the package's terrain tables and loads the atlases.
    ///
    /// Atlases are decoded with [`Image::from_buffer`] from the extension of `atlas_path` (`dds` or
    /// `ktx2`), as colour data (sRGB) with the mip chain the file carries and the sampler the
    /// terrain LOD atlas is published with: linear filtering, trilinear mips, clamped edges, and the
    /// stock terrain layers' 16x anisotropic filtering.
    /// Compressed formats are decoded for a BC-capable GPU, which every desktop adapter the cluster
    /// path runs on is.
    #[cfg(test)]
    pub fn load(
        base: &Path,
        package: &Path,
        images: &mut Assets<Image>,
        materials: &mut Assets<ClusterTerrainMaterial>,
    ) -> Result<Self, String> {
        // `package` is the overlay folder (tables and atlases); the binding to `base` is checked
        // before the overlay database is attached.
        let connection = shared::cluster_overlay::open_overlay_database(base, package)
            .map_err(|error| format!("{error:#}"))?;
        Self::load_connection(package, connection, images, materials)
    }

    /// Uses the launch's binding receipt, including its cheap core identity check on this
    /// connection. No repeated manifest hashing or parsing is needed to load the terrain.
    pub(crate) fn load_certified(
        envelope: &super::envelope::ClusterEnvelope,
        images: &mut Assets<Image>,
        materials: &mut Assets<ClusterTerrainMaterial>,
    ) -> Result<Self, String> {
        let connection = envelope
            .connection()
            .map_err(|error| format!("{error:#}"))?;
        let terrain = Self::load_connection_with(
            &envelope.overlay,
            connection,
            images,
            materials,
            |relative| {
                let key = relative.to_string_lossy().replace('\\', "/");
                let payload = envelope
                    .terrain_atlas_payload(&key)
                    .map_err(|error| format!("{error:#}"))?;
                load_atlas(&envelope.overlay.join(relative), &payload)
            },
        )?;
        terrain.validate_cache_digest(
            envelope
                .terrain_cell_cache_digest()
                .map_err(|error| format!("{error:#}"))?,
            &envelope.root.join(CELL_CACHE_FILE),
        )?;
        Ok(terrain)
    }

    // Direct disk reads exist only for the original metadata/decoder fixture tests.
    #[cfg(test)]
    fn load_connection(
        package: &Path,
        connection: Connection,
        images: &mut Assets<Image>,
        materials: &mut Assets<ClusterTerrainMaterial>,
    ) -> Result<Self, String> {
        Self::load_connection_with(package, connection, images, materials, |relative| {
            let path = package.join(relative);
            let bytes =
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            load_atlas(&path, &HashedPayload::new(bytes.into()))
        })
    }

    fn load_connection_with(
        package: &Path,
        connection: Connection,
        images: &mut Assets<Image>,
        materials: &mut Assets<ClusterTerrainMaterial>,
        mut atlas_image: impl FnMut(&Path) -> Result<Image, String>,
    ) -> Result<Self, String> {
        let metadata = read_metadata(&connection)?;
        drop(connection);
        let mut atlases: HashMap<PathBuf, Handle<Image>> = HashMap::new();
        let mut blocks = Vec::with_capacity(metadata.blocks.len());
        for block in metadata.blocks {
            let atlas_path = package.join(&block.atlas_relative);
            let atlas = match atlases.get(&atlas_path) {
                Some(handle) => handle.clone(),
                None => {
                    let handle = images.add(atlas_image(&block.atlas_relative)?);
                    atlases.insert(atlas_path, handle.clone());
                    handle
                }
            };
            let material = materials.add(ClusterTerrainMaterial {
                base: atlas_material(atlas),
                extension: TerrainClipExtension::open(block.side_cells),
            });
            blocks.push(TerrainBlock {
                mesh_hash: block.mesh_hash,
                primitive_index: block.primitive_index,
                worldspace_id: block.worldspace_id,
                anchor: block.anchor,
                side_cells: block.side_cells,
                instance_path: block.instance_relative,
                cells: block.cells,
                material,
                batch: None,
            });
        }
        let mut by_mesh: HashMap<String, Vec<(u32, usize)>> = HashMap::new();
        for (index, block) in blocks.iter().enumerate() {
            by_mesh
                .entry(block.mesh_hash.clone())
                .or_default()
                .push((block.primitive_index, index));
        }
        Ok(Self {
            worldspaces: blocks.iter().map(|block| block.worldspace_id).collect(),
            blocks,
            heights: CanonicalTerrainHeights(Arc::new(metadata.heights)),
            by_mesh,
            build: metadata.build,
            near: None,
        })
    }

    /// Checks that the package was built from `dataset_root`'s cell cache: the SHA-256 of its
    /// `cell_cache.rkyv` must be the `cell_cache_hash` the package records. A package built from
    /// another dataset has other canonical heights and other cell grids than the terrain the
    /// engine streams, so the near terrain must not take its heights from it.
    #[cfg(test)]
    pub fn validate_dataset(&self, dataset_root: &Path) -> Result<(), String> {
        let path = dataset_root.join(CELL_CACHE_FILE);
        self.validate_cache_digest(sha256_file(&path)?, &path)
    }

    fn validate_cache_digest(&self, actual: [u8; 32], path: &Path) -> Result<(), String> {
        if actual == self.build.cell_cache_hash {
            Ok(())
        } else {
            Err(format!(
                "the cluster terrain was built from cell cache {} but {} is {}",
                hex(&self.build.cell_cache_hash),
                path.display(),
                hex(&actual)
            ))
        }
    }

    /// The block built from a cluster mesh, if that mesh is terrain.
    pub fn block_for_mesh(&self, hash: &str, primitive: u32) -> Option<&TerrainBlock> {
        let index = self
            .by_mesh
            .get(hash)?
            .iter()
            .find(|(candidate, _)| *candidate == primitive)?
            .1;
        self.blocks.get(index)
    }

    /// Records the cluster batch a terrain mesh draws in. Returns whether the mesh is terrain.
    pub fn register_batch(&mut self, hash: &str, primitive: u32, batch: u32) -> bool {
        let Some(index) = self
            .by_mesh
            .get(hash)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(candidate, _)| *candidate == primitive)
            })
            .map(|entry| entry.1)
        else {
            return false;
        };
        self.blocks[index].batch = Some(batch);
        true
    }

    /// The canonical 33x33 heights of one cell (row-major, as the package stores them), keyed by
    /// the absolute cell grid.
    pub fn canonical_heights(&self, world: u32, grid: IVec2) -> Option<&[f32]> {
        self.heights.get(world, grid)
    }

    /// Capture the same immutable allocation, without copying height grids or
    /// retaining terrain materials and mutable coverage state.
    #[cfg(test)]
    pub(crate) fn canonical_height_snapshot(&self) -> CanonicalTerrainHeights {
        self.heights.clone()
    }

    /// Source availability is indexed once, including constant-time negative lookups.
    pub(crate) fn has_world(&self, world: u32) -> bool {
        self.worldspaces.contains(&world)
    }

    /// A terrain with exactly `blocks` and no heights, meshes or near coverage, for tests of
    /// the readers of `blocks` in other modules.
    #[cfg(test)]
    pub(crate) fn for_blocks_test(blocks: Vec<TerrainBlock>) -> Self {
        Self {
            worldspaces: blocks
                .iter()
                .filter(|block| !block.cells.is_empty())
                .map(|block| block.worldspace_id)
                .collect(),
            blocks,
            heights: CanonicalTerrainHeights(Arc::new(HashMap::new())),
            by_mesh: HashMap::new(),
            build: TerrainBuild {
                cell_cache_hash: [0; 32],
            },
            near: None,
        }
    }

    /// Sets what each block clips.
    ///
    /// * `world` is the worldspace the engine draws terrain for. Blocks of any other worldspace,
    ///   and every block when it is `None` (an interior, a stopped stream, no active world), are
    ///   disabled: they discard in every pass.
    /// * `covered` holds `(absolute cell grid, quadrant)` for each quadrant of stock terrain that
    ///   is ready in full detail. Quadrants are numbered `x + 2 * y` (south-west 0, south-east 1,
    ///   north-west 2, north-east 3). Entries outside every block are ignored.
    /// * `origin` is the engine's render origin (the cell grid the render space is centred on),
    ///   which the blocks' runtime positions are relative to.
    ///
    /// Materials are touched only when their settings change, and a call with the same inputs as
    /// the last one does nothing. Returns whether any material changed.
    pub fn set_near_coverage(
        &mut self,
        world: impl Into<Option<u32>>,
        covered: &HashSet<(IVec2, u8)>,
        origin: IVec2,
        readiness: &TerrainDrawReadiness,
        materials: &mut Assets<ClusterTerrainMaterial>,
    ) -> bool {
        let world = world.into();
        let ready: Vec<bool> = self
            .blocks
            .iter()
            .map(|block| block.batch.is_some_and(|batch| readiness.contains(batch)))
            .collect();
        if self.near.as_ref().is_some_and(|near| {
            near.world == world
                && near.origin == origin
                && near.covered == *covered
                && near.ready == ready
        }) {
            return false;
        }
        let mut changed = false;
        for (block, &block_ready) in self.blocks.iter().zip(&ready) {
            let settings =
                TerrainClipSettings::for_block(block, world, origin, covered, block_ready);
            let unchanged = materials
                .get(&block.material)
                .is_some_and(|material| material.extension.settings == settings);
            if unchanged {
                continue;
            }
            if let Some(mut material) = materials.get_mut(&block.material) {
                material.extension.settings = settings;
                changed = true;
            }
        }
        self.near = Some(NearInputs {
            world,
            origin,
            covered: covered.clone(),
            ready,
        });
        changed
    }
}

/// The batches whose terrain the renderer can actually draw: the camera, prepass and shadow
/// pipelines and the material are all ready. The renderer publishes into it, the streaming layer
/// reads it, and clones share one set, so the render world's copy is the same state.
///
/// [`revision`](Self::revision) changes exactly when the set does; marking a batch that is already
/// there or clearing an empty set leaves it alone, so a reader can poll it cheaply.
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct TerrainDrawReadiness {
    ready: Arc<Mutex<HashSet<u32>>>,
    revision: Arc<AtomicU64>,
}

impl TerrainDrawReadiness {
    /// Publishes `batch` as drawable. Returns whether it was newly added.
    pub fn mark(&self, batch: u32) -> bool {
        let mut ready = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        let added = ready.insert(batch);
        if added {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        added
    }

    /// Withdraws every batch. Returns whether there was any.
    pub fn clear(&self) -> bool {
        let mut ready = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        if ready.is_empty() {
            return false;
        }
        ready.clear();
        self.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

    pub fn contains(&self, batch: u32) -> bool {
        self.ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&batch)
    }

    /// A counter that changes whenever the set of drawable batches does.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
}

/// Registers the terrain shaders, the terrain material type and [`TerrainDrawReadiness`]. Added by
/// the package plugin only when terrain is requested, so other runs register nothing
/// of this.
pub struct ClusterTerrainPlugin;

impl Plugin for ClusterTerrainPlugin {
    fn build(&self, app: &mut App) {
        // The clip is an importable library (`cluster_terrain::clip`), loaded the way Bevy loads
        // its own; the two fragment stages import it.
        bevy::shader::load_shader_library!(app, "shaders/terrain_clip.wgsl");
        bevy::asset::embedded_asset!(app, "shaders/terrain_material.wgsl");
        bevy::asset::embedded_asset!(app, "shaders/terrain_prepass.wgsl");
        app.add_plugins(MaterialPlugin::<ClusterTerrainMaterial>::default())
            .init_resource::<TerrainDrawReadiness>();
    }
}

/// The per-block clip settings, bindless where the device allows it (the same scheme as the snow
/// extension: a storage array at binding 101, indexed through the table at binding 100), so the
/// blocks share bind groups and the cluster path draws them in few indirect calls.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
#[data(50, TerrainClipSettings, binding_array(101))]
#[bindless(index_table(range(50..51), binding(100)))]
pub struct TerrainClipExtension {
    settings: TerrainClipSettings,
}

impl From<&TerrainClipExtension> for TerrainClipSettings {
    fn from(extension: &TerrainClipExtension) -> Self {
        extension.settings.clone()
    }
}

impl TerrainClipExtension {
    /// The settings of a block that has not been given any coverage yet: active, nothing clipped.
    fn open(side_cells: u32) -> Self {
        Self {
            settings: TerrainClipSettings::open(side_cells),
        }
    }
}

impl Default for TerrainClipExtension {
    fn default() -> Self {
        Self::open(0)
    }
}

impl MaterialExtension for TerrainClipExtension {
    fn fragment_shader() -> ShaderRef {
        "embedded://engine/cluster/shaders/terrain_material.wgsl".into()
    }

    fn prepass_fragment_shader() -> ShaderRef {
        "embedded://engine/cluster/shaders/terrain_prepass.wgsl".into()
    }

    /// An alpha mask with cutoff 0: see the module documentation.
    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Mask(0.0))
    }
}

/// The uniform's layout, which `terrain_clip.wgsl` declares field for field.
#[derive(ShaderType, Reflect, Debug, Clone, PartialEq)]
struct TerrainClipSettings {
    /// `xy`: the block's runtime origin - the x of its west edge and the z of its north edge.
    /// `z`: the edge of one quadrant. `w`: the quadrants along one side of the block.
    origin_and_size: Vec4,
    /// `x`: 1 while the block's worldspace is the active one, else 0 (the block discards all).
    state: Vec4,
    /// 32 rows of 32 quadrant bits, north-most row first and west-most column in bit 0; row `r`
    /// is `rows[r / 4][r % 4]`. A set bit is a quadrant the stock terrain draws.
    rows: [UVec4; MASK_WORDS],
}

impl Default for TerrainClipSettings {
    fn default() -> Self {
        Self::open(0)
    }
}

impl TerrainClipSettings {
    /// Active, nothing clipped. The origin does not matter while the mask is empty.
    fn open(side_cells: u32) -> Self {
        Self {
            origin_and_size: Vec4::new(0.0, 0.0, CELL_SIZE * 0.5, (side_cells * 2) as f32),
            state: Vec4::new(1.0, 0.0, 0.0, 0.0),
            rows: [UVec4::ZERO; MASK_WORDS],
        }
    }

    /// The settings of `block` for the given active world, render origin and ready quadrants. A
    /// block the renderer cannot draw completely yet (`ready` false) discards everything, so the
    /// stock terrain it will replace is the only terrain on screen until then.
    fn for_block(
        block: &TerrainBlock,
        active_world: Option<u32>,
        origin: IVec2,
        covered: &HashSet<(IVec2, u8)>,
        ready: bool,
    ) -> Self {
        Self::for_geometry(block.into(), active_world, origin, covered, ready)
    }

    fn for_geometry(
        block: TerrainClipGeometry<'_>,
        active_world: Option<u32>,
        origin: IVec2,
        covered: &HashSet<(IVec2, u8)>,
        ready: bool,
    ) -> Self {
        let runtime = block_origin(block.anchor, block.side_cells, origin);
        let mut settings = Self::open(block.side_cells);
        settings.origin_and_size.x = runtime.x;
        settings.origin_and_size.y = runtime.y;
        if !ready || active_world != Some(block.worldspace_id) {
            settings.state.x = 0.0;
            return settings;
        }
        let mut rows = [0u32; MASK_ROWS];
        for &cell in block.cells {
            for quadrant in 0..4u8 {
                if !covered.contains(&(cell, quadrant)) {
                    continue;
                }
                if let Some((row, column)) =
                    quadrant_slot(block.anchor, block.side_cells, cell, quadrant)
                {
                    rows[row as usize] |= 1 << column;
                }
            }
        }
        for (word, chunk) in settings.rows.iter_mut().zip(rows.as_chunks::<4>().0) {
            *word = UVec4::new(chunk[0], chunk[1], chunk[2], chunk[3]);
        }
        settings
    }

    /// The CPU mirror of `terrain_clip` in `terrain_clip.wgsl`, at a runtime x and z.
    #[cfg(test)]
    fn clips(&self, world_x: f32, world_z: f32) -> bool {
        if self.state.x < 0.5 {
            return true;
        }
        let local =
            (Vec2::new(world_x, world_z) - self.origin_and_size.xy()) / self.origin_and_size.z;
        let side = self.origin_and_size.w as i32;
        let column = local.x.floor() as i32;
        let row = local.y.floor() as i32;
        if column < 0 || row < 0 || column >= side || row >= side {
            return false;
        }
        let word = self.rows[row as usize / 4][row as usize % 4];
        (word >> column as u32) & 1 != 0
    }
}

/// The runtime position of a block's west edge (x) and north edge (z), in the render space around
/// `origin`. +x is east and Creation's north (+grid y) runs towards -z, so the north edge of a
/// block whose south-west cell is `anchor` is `side_cells` cells above it.
fn block_origin(anchor: IVec2, side_cells: u32, origin: IVec2) -> Vec2 {
    let east = i64::from(anchor.x) - i64::from(origin.x);
    let north = i64::from(anchor.y) + i64::from(side_cells) - i64::from(origin.y);
    Vec2::new(east as f32 * CELL_SIZE, -(north as f32) * CELL_SIZE)
}

/// Where a quadrant of a cell sits in a block's mask, as `(row, column)`, or `None` when the cell
/// is outside the block or the quadrant is not 0..=3. Columns count quadrants east from the
/// block's west edge; rows count them *south* from its north edge, because runtime z grows
/// southwards while the cell grid's y grows north.
fn quadrant_slot(anchor: IVec2, side_cells: u32, cell: IVec2, quadrant: u8) -> Option<(u32, u32)> {
    if quadrant > 3 || side_cells == 0 || side_cells > MAX_BLOCK_SIDE_CELLS {
        return None;
    }
    let side = i64::from(side_cells);
    let east = i64::from(cell.x) - i64::from(anchor.x);
    let north = i64::from(cell.y) - i64::from(anchor.y);
    if !(0..side).contains(&east) || !(0..side).contains(&north) {
        return None;
    }
    let column = east * 2 + i64::from(quadrant & 1);
    let from_south = north * 2 + i64::from(quadrant >> 1);
    let row = side * 2 - 1 - from_south;
    Some((row as u32, column as u32))
}

/// The material every block draws with: the atlas, lit like the terrain LOD's.
fn atlas_material(atlas: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(atlas),
        metallic: 0.0,
        perceptual_roughness: ATLAS_ROUGHNESS as f32,
        // See the module documentation: a mask with cutoff 0, not an opaque material.
        alpha_mode: AlphaMode::Mask(0.0),
        ..default()
    }
}

/// Decodes one atlas file and checks it is what the terrain LOD atlas is: a square, mipmapped 2D
/// colour image.
fn load_atlas(path: &Path, payload: &HashedPayload) -> Result<Image, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("{}: the atlas has no file extension", path.display()))?;
    decode_atlas(payload.bytes(), &extension)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn decode_atlas(bytes: &[u8], extension: &str) -> Result<Image, String> {
    let image = Image::from_buffer(
        bytes,
        ImageType::Extension(extension),
        CompressedImageFormats::BC,
        true,
        // Linear with trilinear mips like the stock terrain layers, and their 16x anisotropic
        // filtering too (`crate::render::terrain_layer_sampler`): without it the atlas blurs
        // at the grazing angles distant terrain is mostly seen at.
        ImageSampler::Descriptor(ImageSamplerDescriptor {
            anisotropy_clamp: 16,
            ..ImageSamplerDescriptor::linear()
        }),
        RenderAssetUsages::RENDER_WORLD,
    )
    .map_err(|error| error.to_string())?;
    let descriptor = &image.texture_descriptor;
    let size = descriptor.size;
    if descriptor.dimension != TextureDimension::D2 || size.depth_or_array_layers != 1 {
        return Err("the atlas is not a plain 2D image".into());
    }
    if size.width == 0 || size.width != size.height {
        return Err(format!(
            "the atlas is {}x{}, not a square",
            size.width, size.height
        ));
    }
    if size.width > 1 && descriptor.mip_level_count <= 1 {
        return Err("the atlas has no mip chain".into());
    }
    Ok(image)
}

// --- package metadata -----------------------------------------------------------------------

struct RawBlock {
    mesh_hash: String,
    primitive_index: u32,
    worldspace_id: u32,
    anchor: IVec2,
    side_cells: u32,
    instance_relative: String,
    atlas_relative: PathBuf,
    cells: Vec<IVec2>,
}

struct Metadata {
    blocks: Vec<RawBlock>,
    heights: HashMap<(u32, i32, i32), Vec<f32>>,
    build: TerrainBuild,
}

#[cfg(test)]
fn read_metadata_at(database: &Path) -> Result<Metadata, String> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("{}: {error}", database.display()))?;
    read_metadata(&connection)
}

fn read_metadata(connection: &Connection) -> Result<Metadata, String> {
    let build = read_build(connection)?;
    let blocks = read_blocks(connection)?;
    let heights = read_heights(connection)?;
    check_coverage(&blocks, &heights)?;
    Ok(Metadata {
        blocks,
        heights,
        build,
    })
}

fn read_build(connection: &Connection) -> Result<TerrainBuild, String> {
    let mut statement = connection
        .prepare("SELECT version, cell_cache_hash FROM cluster_terrain_build")
        .map_err(|error| format!("cluster_terrain_build: {error}"))?;
    let mut rows = statement
        .query_map([], |row| {
            let version: i64 = row.get(0)?;
            let hash = match row.get_ref(1)? {
                ValueRef::Text(text) => parse_hash(text),
                ValueRef::Blob(blob) => <[u8; 32]>::try_from(blob)
                    .map_err(|_| format!("cell_cache_hash is {} bytes, not 32", blob.len())),
                _ => Err("cell_cache_hash is neither text nor a blob".to_owned()),
            };
            Ok((version, hash))
        })
        .map_err(|error| format!("cluster_terrain_build: {error}"))?;
    let (version, hash) = rows
        .next()
        .ok_or("cluster_terrain_build has no row")?
        .map_err(|error| format!("cluster_terrain_build: {error}"))?;
    if rows.next().is_some() {
        return Err("cluster_terrain_build has more than one row".into());
    }
    if version != TERRAIN_BUILD_VERSION {
        return Err(format!(
            "cluster_terrain_build version {version}, this engine reads {TERRAIN_BUILD_VERSION}"
        ));
    }
    Ok(TerrainBuild {
        cell_cache_hash: hash.map_err(|error| format!("cluster_terrain_build: {error}"))?,
    })
}

/// One `cluster_terrain` row as the database holds it, before any check.
struct BlockRow {
    mesh_hash: String,
    primitive_index: i64,
    worldspace_id: i64,
    anchor_x: i64,
    anchor_y: i64,
    side_cells: i64,
    instance_path: String,
    atlas_path: String,
    source_cells: String,
}

fn read_blocks(connection: &Connection) -> Result<Vec<RawBlock>, String> {
    let mut statement = connection
        .prepare(
            "SELECT mesh_hash, primitive_index, worldspace_id, anchor_x, anchor_y, side_cells,
                    instance_path, atlas_path, source_cells
             FROM cluster_terrain
             ORDER BY worldspace_id, anchor_y, anchor_x, mesh_hash, primitive_index",
        )
        .map_err(|error| format!("cluster_terrain: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(BlockRow {
                mesh_hash: row.get(0)?,
                primitive_index: row.get(1)?,
                worldspace_id: row.get(2)?,
                anchor_x: row.get(3)?,
                anchor_y: row.get(4)?,
                side_cells: row.get(5)?,
                instance_path: row.get(6)?,
                atlas_path: row.get(7)?,
                source_cells: row.get(8)?,
            })
        })
        .map_err(|error| format!("cluster_terrain: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cluster_terrain: {error}"))?;
    if rows.is_empty() {
        return Err("cluster_terrain has no rows".into());
    }
    let mut seen = HashSet::new();
    let mut blocks = Vec::with_capacity(rows.len());
    for row in rows {
        let block = validate_block(row)?;
        if !seen.insert((block.mesh_hash.clone(), block.primitive_index)) {
            return Err(format!(
                "cluster_terrain lists mesh {} primitive {} twice",
                block.mesh_hash, block.primitive_index
            ));
        }
        blocks.push(block);
    }
    Ok(blocks)
}

fn validate_block(row: BlockRow) -> Result<RawBlock, String> {
    let name = format!(
        "terrain block {} primitive {}",
        row.mesh_hash, row.primitive_index
    );
    if row.mesh_hash.is_empty() {
        return Err("cluster_terrain has a row with an empty mesh_hash".into());
    }
    let primitive_index = u32::try_from(row.primitive_index).map_err(|_| {
        format!(
            "{name}: primitive_index {} out of range",
            row.primitive_index
        )
    })?;
    let worldspace_id = u32::try_from(row.worldspace_id)
        .map_err(|_| format!("{name}: worldspace_id {} out of range", row.worldspace_id))?;
    let anchor = IVec2::new(
        i32::try_from(row.anchor_x).map_err(|_| format!("{name}: anchor_x out of range"))?,
        i32::try_from(row.anchor_y).map_err(|_| format!("{name}: anchor_y out of range"))?,
    );
    let side_cells = u32::try_from(row.side_cells)
        .ok()
        .filter(|side| (1..=MAX_BLOCK_SIDE_CELLS).contains(side))
        .ok_or_else(|| {
            format!(
                "{name}: side_cells {} is not in 1..={MAX_BLOCK_SIDE_CELLS}",
                row.side_cells
            )
        })?;
    let instance_relative = relative_package_path(&row.instance_path)
        .map_err(|error| format!("{name}: instance_path: {error}"))?;
    let atlas_relative = relative_package_path(&row.atlas_path)
        .map_err(|error| format!("{name}: atlas_path: {error}"))?;
    let cells = parse_source_cells(&row.source_cells, anchor, side_cells)
        .map_err(|error| format!("{name}: source_cells: {error}"))?;
    Ok(RawBlock {
        mesh_hash: row.mesh_hash,
        primitive_index,
        worldspace_id,
        anchor,
        side_cells,
        instance_relative: instance_relative.to_string_lossy().replace('\\', "/"),
        atlas_relative,
        cells,
    })
}

/// A package-relative path, or why it is not one: absolute paths, drive letters, `..` and empty
/// components are refused so a hostile database cannot point the engine outside its package.
/// Either separator is accepted; the result uses the platform's.
fn relative_package_path(text: &str) -> Result<PathBuf, String> {
    if text.is_empty() {
        return Err("empty".into());
    }
    if text.contains('\0') {
        return Err("contains a NUL".into());
    }
    let mut path = PathBuf::new();
    for part in text.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            return Err(format!("{text:?} has an empty or `.` component"));
        }
        if part == ".." {
            return Err(format!("{text:?} leaves the package"));
        }
        if part.contains(':') {
            return Err(format!("{text:?} names a drive or stream"));
        }
        path.push(part);
    }
    // `push` of a lone component cannot make an absolute path, but the check is the contract.
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("{text:?} is not a plain relative path"));
    }
    Ok(path)
}

fn parse_source_cells(text: &str, anchor: IVec2, side_cells: u32) -> Result<Vec<IVec2>, String> {
    let pairs: Vec<[i64; 2]> = serde_json::from_str(text)
        .map_err(|error| format!("not a JSON array of [grid_x, grid_y] pairs: {error}"))?;
    if pairs.is_empty() {
        return Err("empty".into());
    }
    let side = i64::from(side_cells);
    let mut seen = HashSet::with_capacity(pairs.len());
    let mut cells = Vec::with_capacity(pairs.len());
    for [x, y] in pairs {
        let east = x - i64::from(anchor.x);
        let north = y - i64::from(anchor.y);
        if !(0..side).contains(&east) || !(0..side).contains(&north) {
            return Err(format!(
                "cell ({x}, {y}) is outside the {side_cells}x{side_cells} block at ({}, {})",
                anchor.x, anchor.y
            ));
        }
        if !seen.insert((x, y)) {
            return Err(format!("cell ({x}, {y}) is listed twice"));
        }
        // Both fit: they are within `side` of an `i32` anchor.
        cells.push(IVec2::new(x as i32, y as i32));
    }
    Ok(cells)
}

/// The height grid of each source cell, by worldspace and cell grid.
type HeightGrids = HashMap<(u32, i32, i32), Vec<f32>>;

fn read_heights(connection: &Connection) -> Result<HeightGrids, String> {
    let mut statement = connection
        .prepare("SELECT worldspace_id, grid_x, grid_y, heights FROM cluster_terrain_heights")
        .map_err(|error| format!("cluster_terrain_heights: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(|error| format!("cluster_terrain_heights: {error}"))?;
    let mut heights = HashMap::new();
    for row in rows {
        let (world, x, y, blob) =
            row.map_err(|error| format!("cluster_terrain_heights: {error}"))?;
        let name = format!("heights of cell ({x}, {y}) in worldspace {world}");
        let key = (
            u32::try_from(world).map_err(|_| format!("{name}: worldspace out of range"))?,
            i32::try_from(x).map_err(|_| format!("{name}: grid_x out of range"))?,
            i32::try_from(y).map_err(|_| format!("{name}: grid_y out of range"))?,
        );
        let values = decode_heights(&blob).map_err(|error| format!("{name}: {error}"))?;
        if heights.insert(key, values).is_some() {
            return Err(format!("{name} are listed twice"));
        }
    }
    Ok(heights)
}

fn decode_heights(blob: &[u8]) -> Result<Vec<f32>, String> {
    let expected = HEIGHT_GRID_SIDE * HEIGHT_GRID_SIDE * 4;
    if blob.len() != expected {
        return Err(format!("{} bytes, expected {expected}", blob.len()));
    }
    blob.as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| {
            let value = f32::from_le_bytes(*bytes);
            if value.is_finite() {
                Ok(value)
            } else {
                Err("a height is not finite".to_owned())
            }
        })
        .collect()
}

/// Every source cell is claimed by one block of its worldspace, has heights, and every heights row
/// belongs to a converted worldspace. Heights cover the whole world to avoid mixed weld policies.
fn check_coverage(
    blocks: &[RawBlock],
    heights: &HashMap<(u32, i32, i32), Vec<f32>>,
) -> Result<(), String> {
    let mut owners: HashMap<(u32, i32, i32), &RawBlock> = HashMap::new();
    for block in blocks {
        for cell in &block.cells {
            let key = (block.worldspace_id, cell.x, cell.y);
            if let Some(other) = owners.insert(key, block) {
                return Err(format!(
                    "cell ({}, {}) of worldspace {} is claimed by terrain blocks {}:{} and {}:{}",
                    cell.x,
                    cell.y,
                    block.worldspace_id,
                    other.mesh_hash,
                    other.primitive_index,
                    block.mesh_hash,
                    block.primitive_index
                ));
            }
            if !heights.contains_key(&key) {
                return Err(format!(
                    "terrain block {}:{} has source cell ({}, {}) with no canonical heights",
                    block.mesh_hash, block.primitive_index, cell.x, cell.y
                ));
            }
        }
    }
    let worlds: HashSet<u32> = blocks.iter().map(|block| block.worldspace_id).collect();
    for &(world, x, y) in heights.keys() {
        if !worlds.contains(&world) {
            return Err(format!(
                "canonical heights of cell ({x}, {y}) in worldspace {world} belong to no terrain block worldspace"
            ));
        }
    }
    Ok(())
}

fn parse_hash(text: &[u8]) -> Result<[u8; 32], String> {
    if text.len() != 64 {
        return Err(format!(
            "cell_cache_hash is {} characters, not 64",
            text.len()
        ));
    }
    let mut hash = [0u8; 32];
    for (byte, pair) in hash.iter_mut().zip(text.as_chunks::<2>().0) {
        let digit = |c: u8| (c as char).to_digit(16).map(|value| value as u8);
        let (Some(high), Some(low)) = (digit(pair[0]), digit(pair[1])) else {
            return Err("cell_cache_hash is not hexadecimal".into());
        };
        *byte = high << 4 | low;
    }
    Ok(hash)
}

fn hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
fn sha256_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

impl TerrainViewBlock {
    /// Append forward then depth/shadow clip settings, exactly twenty vec4 words.
    /// Coverage is independently proven per pass in this same retained view.
    pub fn append_view_records(
        &self,
        world: Option<u32>,
        origin: IVec2,
        forward_covered: &HashSet<(IVec2, u8)>,
        depth_covered: &HashSet<(IVec2, u8)>,
        ready: bool,
        output: &mut Vec<u8>,
    ) {
        self.append_clip_record(world, origin, forward_covered, ready, output);
        self.append_clip_record(world, origin, depth_covered, ready, output);
    }
}

#[cfg(test)]
impl ClusterTerrain {
    pub(crate) fn for_heights_test(world: u32, grid: IVec2, heights: Vec<f32>) -> Self {
        let mut terrain = Self::for_blocks_test(Vec::new());
        terrain.heights = CanonicalTerrainHeights(Arc::new(HashMap::from([(
            (world, grid.x, grid.y),
            heights,
        )])));
        terrain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_material_needs_no_tangent_inputs() {
        let needs =
            super::super::material_geometry::requirements(&atlas_material(Handle::default()));
        assert!(
            needs.supported && !needs.tangents,
            "terrain tangent skipping requires a colour-only atlas material"
        );
    }

    const HALF: f32 = CELL_SIZE * 0.5;

    fn block(world: u32, anchor: IVec2, side: u32, cells: &[IVec2]) -> TerrainBlock {
        TerrainBlock {
            mesh_hash: "aa".into(),
            primitive_index: 0,
            worldspace_id: world,
            anchor,
            side_cells: side,
            instance_path: "terrain/aa.mci".into(),
            cells: cells.to_vec(),
            material: Handle::default(),
            batch: None,
        }
    }

    fn all_cells(anchor: IVec2, side: u32) -> Vec<IVec2> {
        (0..side as i32)
            .flat_map(|y| (0..side as i32).map(move |x| anchor + IVec2::new(x, y)))
            .collect()
    }

    fn covered(entries: &[(IVec2, u8)]) -> HashSet<(IVec2, u8)> {
        entries.iter().copied().collect()
    }

    /// The runtime centre of a quadrant of a cell, in a render space around `origin`.
    fn quadrant_centre(cell: IVec2, quadrant: u8, origin: IVec2) -> (f32, f32) {
        let east = (cell.x - origin.x) as f32 * CELL_SIZE + (f32::from(quadrant & 1) + 0.5) * HALF;
        // North is -z: the cell's north edge is at -(gy + 1 - oy) cells and its south edge one
        // cell nearer.
        let south_edge = -((cell.y - origin.y) as f32) * CELL_SIZE;
        let z = south_edge - (f32::from(quadrant >> 1) + 0.5) * HALF;
        (east, z)
    }

    #[test]
    fn quadrant_slots_count_columns_east_and_rows_south_from_the_north_edge() {
        let anchor = IVec2::new(10, 20);
        // South-west quadrant of the south-west cell: west-most column, south-most row.
        assert_eq!(quadrant_slot(anchor, 2, anchor, 0), Some((3, 0)));
        // North-east quadrant of the north-east cell: east-most column, north-most row.
        assert_eq!(
            quadrant_slot(anchor, 2, IVec2::new(11, 21), 3),
            Some((0, 3))
        );
        // South-east and north-west of the south-west cell.
        assert_eq!(quadrant_slot(anchor, 2, anchor, 1), Some((3, 1)));
        assert_eq!(quadrant_slot(anchor, 2, anchor, 2), Some((2, 0)));
    }

    #[test]
    fn negative_grids_use_the_same_offsets() {
        let anchor = IVec2::new(-3, -5);
        assert_eq!(quadrant_slot(anchor, 1, anchor, 2), Some((0, 0)));
        assert_eq!(quadrant_slot(anchor, 1, anchor, 1), Some((1, 1)));
        let origin = block_origin(anchor, 1, anchor);
        assert_eq!(origin, Vec2::new(0.0, -CELL_SIZE));
        let far = block_origin(IVec2::new(-3, -5), 4, IVec2::new(2, -7));
        assert_eq!(far, Vec2::new(-5.0 * CELL_SIZE, -6.0 * CELL_SIZE));
    }

    #[test]
    fn slots_outside_the_block_or_the_quadrant_range_do_not_exist() {
        let anchor = IVec2::new(0, 0);
        assert_eq!(quadrant_slot(anchor, 2, IVec2::new(2, 0), 0), None);
        assert_eq!(quadrant_slot(anchor, 2, IVec2::new(0, 2), 0), None);
        assert_eq!(quadrant_slot(anchor, 2, IVec2::new(-1, 0), 0), None);
        assert_eq!(quadrant_slot(anchor, 2, anchor, 4), None);
        assert_eq!(quadrant_slot(anchor, 0, anchor, 0), None);
        assert_eq!(quadrant_slot(anchor, 17, anchor, 0), None);
        // The last quadrant of the largest block still fits the 32x32 mask.
        assert_eq!(
            quadrant_slot(anchor, 16, IVec2::new(15, 15), 3),
            Some((0, 31))
        );
    }

    #[test]
    fn the_clip_follows_the_covered_quadrants_on_the_runtime_grid() {
        let anchor = IVec2::new(-1, -1);
        let cells = all_cells(anchor, 2);
        let block = block(7, anchor, 2, &cells);
        let origin = IVec2::new(0, 0);
        let ready = covered(&[(IVec2::new(-1, -1), 0), (IVec2::new(0, 0), 3)]);
        let settings = TerrainClipSettings::for_block(&block, Some(7), origin, &ready, true);
        for &cell in &cells {
            for quadrant in 0..4u8 {
                let (x, z) = quadrant_centre(cell, quadrant, origin);
                assert_eq!(
                    settings.clips(x, z),
                    ready.contains(&(cell, quadrant)),
                    "cell {cell} quadrant {quadrant}"
                );
            }
        }
    }

    #[test]
    fn positions_outside_the_block_are_never_clipped() {
        let anchor = IVec2::new(0, 0);
        let block = block(1, anchor, 1, &[anchor]);
        let all = covered(&[(anchor, 0), (anchor, 1), (anchor, 2), (anchor, 3)]);
        let settings = TerrainClipSettings::for_block(&block, Some(1), IVec2::ZERO, &all, true);
        // The block spans x 0..4096 and z -4096..0.
        assert!(settings.clips(1.0, -1.0));
        assert!(settings.clips(CELL_SIZE - 1.0, -CELL_SIZE + 1.0));
        assert!(!settings.clips(-1.0, -1.0));
        assert!(!settings.clips(CELL_SIZE, -1.0));
        assert!(!settings.clips(1.0, 1.0));
        assert!(!settings.clips(1.0, -CELL_SIZE - 1.0));
    }

    #[test]
    fn a_boundary_belongs_to_the_quadrant_it_starts() {
        let anchor = IVec2::new(0, 0);
        let block = block(1, anchor, 1, &[anchor]);
        let south_west = covered(&[(anchor, 0)]);
        let settings =
            TerrainClipSettings::for_block(&block, Some(1), IVec2::ZERO, &south_west, true);
        // South-west quadrant: x 0..2048, z -2048..0 (south is +z).
        assert!(settings.clips(0.0, -1.0));
        assert!(settings.clips(HALF - 1.0, -HALF + 1.0));
        assert!(!settings.clips(HALF, -1.0));
        assert!(!settings.clips(1.0, -HALF - 1.0));
    }

    #[test]
    fn rebasing_the_render_origin_moves_the_clip_with_the_world() {
        let anchor = IVec2::new(40, 12);
        let cells = all_cells(anchor, 2);
        let block = block(1, anchor, 2, &cells);
        let ready = covered(&[(IVec2::new(41, 13), 1)]);
        for origin in [IVec2::new(40, 12), IVec2::new(35, 20), IVec2::new(-100, 3)] {
            let settings = TerrainClipSettings::for_block(&block, Some(1), origin, &ready, true);
            let (x, z) = quadrant_centre(IVec2::new(41, 13), 1, origin);
            assert!(settings.clips(x, z), "origin {origin}");
            let (x, z) = quadrant_centre(IVec2::new(41, 13), 0, origin);
            assert!(!settings.clips(x, z), "origin {origin}");
        }
        // Only the origin changes between rebases, never the mask.
        let near =
            TerrainClipSettings::for_block(&block, Some(1), IVec2::new(40, 12), &ready, true);
        let far = TerrainClipSettings::for_block(&block, Some(1), IVec2::new(35, 20), &ready, true);
        assert_eq!(near.rows, far.rows);
        assert_ne!(near.origin_and_size, far.origin_and_size);
    }

    #[test]
    fn entries_for_cells_the_block_was_not_built_from_are_ignored() {
        let anchor = IVec2::new(0, 0);
        // A 2x2 block built from only one cell.
        let block = block(1, anchor, 2, &[IVec2::new(0, 0)]);
        let ready = covered(&[
            (IVec2::new(1, 1), 0),
            (IVec2::new(5, 5), 0),
            (IVec2::new(0, 0), 7),
        ]);
        let settings = TerrainClipSettings::for_block(&block, Some(1), IVec2::ZERO, &ready, true);
        assert_eq!(settings.rows, [UVec4::ZERO; MASK_WORDS]);
    }

    #[test]
    fn another_world_or_no_world_disables_the_block_in_every_pass() {
        let anchor = IVec2::new(0, 0);
        let block = block(1, anchor, 1, &[anchor]);
        let ready = covered(&[(anchor, 0)]);
        for world in [None, Some(2)] {
            let settings = TerrainClipSettings::for_block(&block, world, IVec2::ZERO, &ready, true);
            assert_eq!(settings.state.x, 0.0, "world {world:?}");
            assert!(settings.clips(1.0, -1.0));
            assert!(settings.clips(1.0e9, 1.0e9), "everywhere, not just inside");
        }
        let active = TerrainClipSettings::for_block(&block, Some(1), IVec2::ZERO, &ready, true);
        assert_eq!(active.state.x, 1.0);
        assert!(!active.clips(HALF + 1.0, -1.0));
    }

    #[test]
    fn a_new_block_draws_everything() {
        let settings = TerrainClipSettings::open(2);
        assert_eq!(settings.state.x, 1.0);
        assert!(!settings.clips(100.0, -100.0));
        assert!(!settings.clips(-5000.0, 5000.0));
        assert_eq!(settings, TerrainClipExtension::open(2).settings);
    }

    #[test]
    fn coverage_is_idempotent_and_reports_only_real_changes() {
        let mut materials = Assets::<ClusterTerrainMaterial>::default();
        let anchor = IVec2::new(0, 0);
        let handle = materials.add(ClusterTerrainMaterial {
            base: StandardMaterial::default(),
            extension: TerrainClipExtension::open(1),
        });
        let mut block = block(1, anchor, 1, &[anchor]);
        block.material = handle.clone();
        block.batch = Some(0);
        let readiness = TerrainDrawReadiness::default();
        readiness.mark(0);
        let mut terrain = ClusterTerrain {
            #[cfg(test)]
            worldspaces: HashSet::from([block.worldspace_id]),
            blocks: vec![block],
            heights: CanonicalTerrainHeights(Arc::new(HashMap::new())),
            by_mesh: HashMap::new(),
            build: TerrainBuild {
                cell_cache_hash: [0; 32],
            },
            near: None,
        };
        let ready = covered(&[(anchor, 2)]);
        assert!(terrain.set_near_coverage(1u32, &ready, IVec2::ZERO, &readiness, &mut materials));
        assert!(!terrain.set_near_coverage(1u32, &ready, IVec2::ZERO, &readiness, &mut materials));
        assert!(!terrain.set_near_coverage(
            Some(1),
            &ready,
            IVec2::ZERO,
            &readiness,
            &mut materials
        ));
        let settings = materials.get(&handle).unwrap().extension.settings.clone();
        let north_west = quadrant_centre(anchor, 2, IVec2::ZERO);
        assert!(settings.clips(north_west.0, north_west.1));
        // A new render origin changes the origin, and so the material.
        assert!(terrain.set_near_coverage(
            1u32,
            &ready,
            IVec2::new(1, 0),
            &readiness,
            &mut materials
        ));
        // No world disables the block; the same disabled state again changes nothing.
        assert!(terrain.set_near_coverage(
            None,
            &ready,
            IVec2::new(1, 0),
            &readiness,
            &mut materials
        ));
        assert!(!terrain.set_near_coverage(
            None,
            &ready,
            IVec2::new(1, 0),
            &readiness,
            &mut materials
        ));
        let disabled = materials.get(&handle).unwrap().extension.settings.clone();
        assert_eq!(disabled.state.x, 0.0);
    }

    #[test]
    fn a_block_the_renderer_cannot_draw_yet_discards_everything() {
        let anchor = IVec2::new(0, 0);
        let mut materials = Assets::<ClusterTerrainMaterial>::default();
        let handle = materials.add(ClusterTerrainMaterial {
            base: StandardMaterial::default(),
            extension: TerrainClipExtension::open(1),
        });
        let mut block = block(1, anchor, 1, &[anchor]);
        block.material = handle.clone();
        block.batch = Some(3);
        let mut terrain = ClusterTerrain {
            #[cfg(test)]
            worldspaces: HashSet::from([block.worldspace_id]),
            blocks: vec![block],
            heights: CanonicalTerrainHeights(Arc::new(HashMap::new())),
            by_mesh: HashMap::new(),
            build: TerrainBuild {
                cell_cache_hash: [0; 32],
            },
            near: None,
        };
        let none = covered(&[]);
        let readiness = TerrainDrawReadiness::default();
        // Active world, nothing covered, but the batch is not published: nothing of it may draw.
        terrain.set_near_coverage(1u32, &none, IVec2::ZERO, &readiness, &mut materials);
        let hidden = materials.get(&handle).unwrap().extension.settings.clone();
        assert_eq!(hidden.state.x, 0.0);
        // Publishing the batch is a change that makes the block draw, and only then.
        readiness.mark(3);
        assert!(terrain.set_near_coverage(1u32, &none, IVec2::ZERO, &readiness, &mut materials));
        let shown = materials.get(&handle).unwrap().extension.settings.clone();
        assert_eq!(shown.state.x, 1.0);
        assert!(!shown.clips(100.0, -100.0));
        // Withdrawing it hides the block again.
        readiness.clear();
        assert!(terrain.set_near_coverage(1u32, &none, IVec2::ZERO, &readiness, &mut materials));
        assert_eq!(
            materials.get(&handle).unwrap().extension.settings.state.x,
            0.0
        );
    }

    #[test]
    fn paths_must_stay_inside_the_package() {
        for good in ["a/b.mci", "terrain\\atlas\\x.ktx2", "x.dds"] {
            assert!(relative_package_path(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "\\windows\\x",
            "Q:/Windows/x.dds",
            "C:x.dds",
            "../x",
            "a/../../x",
            "a\\..\\x",
            "a//b",
            "./a",
            "a/./b",
            "a/b:stream",
            "a\0b",
        ] {
            assert!(relative_package_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn source_cells_must_be_unique_and_inside_the_block() {
        let anchor = IVec2::new(-2, 4);
        let ok = parse_source_cells("[[-2,4],[-1,5]]", anchor, 2).unwrap();
        assert_eq!(ok, vec![IVec2::new(-2, 4), IVec2::new(-1, 5)]);
        for bad in [
            "[]",
            "not json",
            "[[1]]",
            "[[-2,4],[-2,4]]",
            "[[0,4]]",
            "[[-2,6]]",
            "[[-3,4]]",
            "[[-2,3]]",
            "[[1.5,4]]",
            "{\"a\":1}",
        ] {
            assert!(parse_source_cells(bad, anchor, 2).is_err(), "{bad}");
        }
    }

    #[test]
    fn heights_must_be_a_full_finite_grid() {
        let good: Vec<u8> = (0..HEIGHT_GRID_SIDE * HEIGHT_GRID_SIDE)
            .flat_map(|index| (index as f32).to_le_bytes())
            .collect();
        let values = decode_heights(&good).unwrap();
        assert_eq!(values.len(), 33 * 33);
        assert_eq!(values[40], 40.0);
        assert!(decode_heights(&good[..good.len() - 4]).is_err());
        assert!(decode_heights(&[]).is_err());
        let mut nan = good.clone();
        nan[8..12].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_heights(&nan).unwrap_err().contains("finite"));
        let mut infinite = good;
        infinite[0..4].copy_from_slice(&f32::INFINITY.to_le_bytes());
        assert!(decode_heights(&infinite).is_err());
    }

    const SCHEMA: &str = "
        CREATE TABLE cluster_terrain (
            mesh_hash TEXT NOT NULL, primitive_index INTEGER NOT NULL,
            worldspace_id INTEGER NOT NULL, anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
            side_cells INTEGER NOT NULL, instance_path TEXT NOT NULL, atlas_path TEXT NOT NULL,
            source_cells TEXT NOT NULL, PRIMARY KEY (mesh_hash, primitive_index));
        CREATE TABLE cluster_terrain_heights (
            worldspace_id INTEGER NOT NULL, grid_x INTEGER NOT NULL, grid_y INTEGER NOT NULL,
            heights BLOB NOT NULL, PRIMARY KEY (worldspace_id, grid_x, grid_y));
        CREATE TABLE cluster_terrain_build (version INTEGER NOT NULL, cell_cache_hash TEXT NOT NULL);";

    fn heights_blob() -> Vec<u8> {
        vec![0u8; HEIGHT_GRID_SIDE * HEIGHT_GRID_SIDE * 4]
    }

    /// An overlay database with one 2x2 block at (-1, -1) built from cells (-1,-1) and (0,-1).
    fn fixture(directory: &Path, mutate: impl FnOnce(&Connection)) -> PathBuf {
        let path = directory.join("clusters.db");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
            .execute(
                "INSERT INTO cluster_terrain VALUES ('aa', 0, 7, -1, -1, 2, 'terrain/aa.mci',
                    'terrain/aa.ktx2', '[[-1,-1],[0,-1]]')",
                [],
            )
            .unwrap();
        for grid_x in [-1, 0] {
            connection
                .execute(
                    "INSERT INTO cluster_terrain_heights VALUES (7, ?1, -1, ?2)",
                    rusqlite::params![grid_x, heights_blob()],
                )
                .unwrap();
        }
        connection
            .execute(
                &format!(
                    "INSERT INTO cluster_terrain_build VALUES (1, '{}')",
                    "ab".repeat(32)
                ),
                [],
            )
            .unwrap();
        mutate(&connection);
        path
    }

    #[test]
    fn a_consistent_package_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path(), |_| {});
        let metadata = read_metadata_at(&path).unwrap();
        assert_eq!(metadata.blocks.len(), 1);
        let block = &metadata.blocks[0];
        assert_eq!(block.anchor, IVec2::new(-1, -1));
        assert_eq!(block.side_cells, 2);
        assert_eq!(block.cells, vec![IVec2::new(-1, -1), IVec2::new(0, -1)]);
        assert_eq!(block.instance_relative, "terrain/aa.mci");
        assert_eq!(metadata.heights.len(), 2);
        assert_eq!(metadata.build.cell_cache_hash, [0xab; 32]);
    }

    #[test]
    fn cluster_sources_load_terrain_without_baked_sources_and_reject_missing_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(
            root.join("OWNER.txt"),
            "engine cluster terrain source fixture",
        )
        .unwrap();
        let path = fixture(root, |connection| {
            connection
                .execute("UPDATE cluster_terrain SET atlas_path='atlas.ktx2'", [])
                .unwrap();
        });
        // Use the real producer: Bevy treats a one-pixel uncompressed KTX2 as a 1D image.
        let dds = dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 4, 4)
                .with_mip_levels(3),
            &mut dummy_content::rng::Rng::new(0),
        )
        .unwrap();
        let atlas = converter::texture::TextureConverter::convert(
            &dds,
            converter::texture::TextureEncoding::ColorSrgb,
        )
        .unwrap();
        std::fs::write(root.join("atlas.ktx2"), atlas).unwrap();
        let load = || {
            ClusterTerrain::load_connection(
                root,
                Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap(),
                &mut Assets::default(),
                &mut Assets::default(),
            )
        };
        let terrain = load().unwrap();
        assert_eq!(terrain.blocks.len(), 1);
        assert!(terrain.has_world(7));
        assert!(!terrain.has_world(8));
        assert_eq!(
            terrain.canonical_heights(7, IVec2::new(-1, -1)),
            Some(&[0.0; 33 * 33][..])
        );
        std::fs::remove_file(root.join("atlas.ktx2")).unwrap();
        let error = load().err().unwrap();
        assert!(error.contains("atlas.ktx2"), "{error}");
        Connection::open(&path)
            .unwrap()
            .execute("DELETE FROM cluster_terrain_heights WHERE grid_x=0", [])
            .unwrap();
        let error = load().err().unwrap();
        assert!(error.contains("height"), "{error}");
    }

    #[test]
    fn inconsistent_metadata_is_refused_with_a_reason() {
        let cases: [(&str, &str); 9] = [
            (
                "DELETE FROM cluster_terrain_heights WHERE grid_x = 0",
                "no canonical heights",
            ),
            (
                "INSERT INTO cluster_terrain_heights VALUES (99, 5, 5, zeroblob(4356))",
                "belong to no terrain block",
            ),
            (
                "UPDATE cluster_terrain_heights SET heights = zeroblob(10) WHERE grid_x = 0",
                "expected 4356",
            ),
            ("UPDATE cluster_terrain SET side_cells = 17", "side_cells"),
            (
                "UPDATE cluster_terrain SET atlas_path = '../x.ktx2'",
                "leaves the package",
            ),
            (
                "UPDATE cluster_terrain SET source_cells = '[[5,5]]'",
                "outside",
            ),
            ("UPDATE cluster_terrain_build SET version = 2", "version 2"),
            ("DELETE FROM cluster_terrain_build", "no row"),
            ("DELETE FROM cluster_terrain", "no rows"),
        ];
        for (statement, expected) in cases {
            let directory = tempfile::tempdir().unwrap();
            let path = fixture(directory.path(), |connection| {
                connection.execute_batch(statement).unwrap();
            });
            let error = read_metadata_at(&path).err().expect(statement);
            assert!(error.contains(expected), "{statement}: {error}");
        }
    }

    #[test]
    fn two_blocks_may_not_claim_one_cell() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path(), |connection| {
            connection
                .execute_batch(
                    "INSERT INTO cluster_terrain VALUES ('bb', 0, 7, 0, -1, 1, 'terrain/bb.mci',
                        'terrain/bb.ktx2', '[[0,-1]]')",
                )
                .unwrap();
        });
        let error = read_metadata_at(&path).err().unwrap();
        assert!(error.contains("claimed by terrain blocks"), "{error}");
    }

    #[test]
    fn the_same_cell_may_exist_in_two_worldspaces() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path(), |connection| {
            connection
                .execute_batch(
                    "INSERT INTO cluster_terrain VALUES ('bb', 0, 9, -1, -1, 1, 'terrain/bb.mci',
                        'terrain/bb.ktx2', '[[-1,-1]]');
                     INSERT INTO cluster_terrain_heights VALUES (9, -1, -1, zeroblob(4356));",
                )
                .unwrap();
        });
        assert_eq!(read_metadata_at(&path).unwrap().blocks.len(), 2);
    }

    #[test]
    fn a_missing_table_is_named() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE unrelated (x INTEGER)")
            .unwrap();
        let error = read_metadata_at(&path).err().unwrap();
        assert!(error.contains("cluster_terrain_build"), "{error}");
    }

    #[test]
    fn the_build_hash_reads_as_text_or_a_blob() {
        assert_eq!(parse_hash("AB".repeat(32).as_bytes()).unwrap(), [0xab; 32]);
        assert!(parse_hash(b"abc").is_err());
        assert!(parse_hash("zz".repeat(32).as_bytes()).is_err());
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path(), |connection| {
            connection
                .execute(
                    "UPDATE cluster_terrain_build SET cell_cache_hash = ?1",
                    [vec![7u8; 32]],
                )
                .unwrap();
        });
        assert_eq!(
            read_metadata_at(&path).unwrap().build.cell_cache_hash,
            [7; 32]
        );
    }

    #[test]
    fn the_dataset_must_be_the_one_the_package_was_built_from() {
        let dataset = tempfile::tempdir().unwrap();
        std::fs::write(dataset.path().join(CELL_CACHE_FILE), b"cell cache bytes").unwrap();
        let digest: [u8; 32] = Sha256::digest(b"cell cache bytes").into();
        let mut terrain = ClusterTerrain {
            #[cfg(test)]
            worldspaces: HashSet::new(),
            blocks: Vec::new(),
            heights: CanonicalTerrainHeights(Arc::new(HashMap::new())),
            by_mesh: HashMap::new(),
            build: TerrainBuild {
                cell_cache_hash: digest,
            },
            near: None,
        };
        assert_eq!(terrain.validate_dataset(dataset.path()), Ok(()));
        terrain.build.cell_cache_hash[0] ^= 1;
        let error = terrain.validate_dataset(dataset.path()).unwrap_err();
        assert!(error.contains("was built from cell cache"), "{error}");
        let empty = tempfile::tempdir().unwrap();
        assert!(terrain.validate_dataset(empty.path()).is_err());
    }

    #[test]
    fn mesh_lookup_finds_the_block_and_registers_its_batch() {
        let mut blocks = vec![block(1, IVec2::ZERO, 1, &[IVec2::ZERO])];
        blocks[0].mesh_hash = "cafe".into();
        blocks[0].primitive_index = 2;
        let mut terrain = ClusterTerrain {
            #[cfg(test)]
            worldspaces: blocks.iter().map(|block| block.worldspace_id).collect(),
            by_mesh: HashMap::from([("cafe".to_owned(), vec![(2, 0)])]),
            blocks,
            heights: CanonicalTerrainHeights(Arc::new(HashMap::from([(
                (1, 0, 0),
                vec![0.0; 33 * 33],
            )]))),
            build: TerrainBuild {
                cell_cache_hash: [0; 32],
            },
            near: None,
        };
        assert!(terrain.block_for_mesh("cafe", 2).is_some());
        assert!(terrain.block_for_mesh("cafe", 1).is_none());
        assert!(terrain.block_for_mesh("beef", 2).is_none());
        assert!(!terrain.register_batch("cafe", 1, 9));
        assert!(terrain.register_batch("cafe", 2, 9));
        assert_eq!(terrain.block_for_mesh("cafe", 2).unwrap().batch, Some(9));
        assert_eq!(
            terrain.canonical_heights(1, IVec2::ZERO).unwrap().len(),
            33 * 33
        );
        assert!(terrain.canonical_heights(2, IVec2::ZERO).is_none());
        assert!(terrain.canonical_heights(1, IVec2::new(1, 0)).is_none());
        let captured = terrain.canonical_height_snapshot();
        assert!(captured.same_allocation(&terrain.canonical_height_snapshot()));
        assert!(
            std::ptr::eq(
                terrain.canonical_heights(1, IVec2::ZERO).unwrap().as_ptr(),
                captured.get(1, IVec2::ZERO).unwrap().as_ptr(),
            ),
            "snapshot borrows the very same height sample allocation"
        );
        drop(terrain);
        assert_eq!(captured.get(1, IVec2::ZERO).unwrap(), &[0.0; 33 * 33]);
        assert!(captured.get(2, IVec2::ZERO).is_none());
    }

    #[test]
    fn readiness_revision_changes_only_when_the_set_does() {
        let readiness = TerrainDrawReadiness::default();
        let start = readiness.revision();
        assert!(!readiness.clear(), "clearing an empty set is not a change");
        assert_eq!(readiness.revision(), start);
        assert!(readiness.mark(4));
        let marked = readiness.revision();
        assert_ne!(marked, start);
        assert!(
            !readiness.mark(4),
            "marking a ready batch again is not a change"
        );
        assert_eq!(readiness.revision(), marked);
        assert!(readiness.contains(4));
        assert!(!readiness.contains(5));
        // A clone is the same state, which is what the render world's extracted copy is.
        let shared = readiness.clone();
        assert!(shared.mark(5));
        assert!(readiness.contains(5));
        assert_eq!(shared.revision(), readiness.revision());
        assert!(readiness.clear());
        assert!(!shared.contains(4));
        assert_ne!(readiness.revision(), marked);
    }

    #[test]
    fn an_atlas_that_is_not_an_image_is_refused() {
        assert!(decode_atlas(b"not an image", "ktx2").is_err());
        assert!(decode_atlas(b"not an image", "dds").is_err());
        assert!(decode_atlas(&[], "bogus").is_err());
    }

    fn record_settings(bytes: &[u8]) -> TerrainClipSettings {
        assert_eq!(bytes.len(), TERRAIN_CLIP_VEC4S as usize * 16);
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        TerrainClipSettings {
            origin_and_size: Vec4::from_array(std::array::from_fn(|i| f32::from_bits(words[i]))),
            state: Vec4::from_array(std::array::from_fn(|i| f32::from_bits(words[4 + i]))),
            rows: std::array::from_fn(|i| {
                UVec4::from_array(std::array::from_fn(|j| words[8 + i * 4 + j]))
            }),
        }
    }

    #[test]
    fn view_record_has_fixed_layout_and_preserves_output_prefix() {
        let anchor = IVec2::new(10, 20);
        let source = block(7, anchor, 2, &[anchor, anchor + IVec2::ONE]);
        let view = TerrainViewBlock::from(&source);
        assert_eq!(view.worldspace_id, source.worldspace_id);
        assert_eq!(view.anchor, source.anchor);
        assert_eq!(view.side_cells, source.side_cells);
        assert_eq!(view.cells, source.cells);
        let ready = covered(&[(anchor, 0), (anchor + IVec2::ONE, 3)]);
        let mut bytes = vec![0xa5; 16];
        view.append_clip_record(Some(7), anchor, &ready, true, &mut bytes);
        assert_eq!(&bytes[..16], &[0xa5; 16]);
        assert_eq!(bytes.len(), 176);
        let record = &bytes[16..];
        let words: Vec<_> = record
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        assert_eq!(
            &words[..4],
            &[
                0.0f32.to_bits(),
                (-2.0 * CELL_SIZE).to_bits(),
                HALF.to_bits(),
                4.0f32.to_bits()
            ]
        );
        assert_eq!(&words[4..8], &[1.0f32.to_bits(), 0, 0, 0]);
        // SW quadrant of SW cell is row 3 bit 0; NE of NE cell is row 0 bit 3.
        assert_eq!(&words[8..12], &[8, 0, 0, 1]);
        assert!(words[12..].iter().all(|word| *word == 0));
        assert_eq!(
            record_settings(record),
            TerrainClipSettings::for_block(&source, Some(7), anchor, &ready, true)
        );
    }

    #[test]
    fn view_records_match_cpu_clips_for_rebased_quadrants_and_inactive_views() {
        let anchor = IVec2::new(-3, 5);
        let cells = all_cells(anchor, 2);
        let source = block(7, anchor, 2, &cells);
        let view = TerrainViewBlock::from(&source);
        let covered = covered(&[(anchor, 0), (anchor + IVec2::ONE, 3)]);
        for origin in [IVec2::ZERO, anchor, IVec2::new(100, -80)] {
            for (world, ready) in [
                (Some(7), true),
                (Some(8), true),
                (None, true),
                (Some(7), false),
            ] {
                let mut bytes = Vec::new();
                view.append_clip_record(world, origin, &covered, ready, &mut bytes);
                let actual = record_settings(&bytes);
                let expected =
                    TerrainClipSettings::for_block(&source, world, origin, &covered, ready);
                assert_eq!(actual, expected);
                for &cell in &cells {
                    for quadrant in 0..4 {
                        let (x, z) = quadrant_centre(cell, quadrant, origin);
                        let clipped =
                            world != Some(7) || !ready || covered.contains(&(cell, quadrant));
                        assert_eq!(actual.clips(x, z), clipped);
                    }
                }
                if world != Some(7) || !ready {
                    assert!(actual.clips(1.0e9, -1.0e9));
                    assert_eq!(&bytes[16..20], &0.0f32.to_bits().to_le_bytes());
                    assert!(bytes[32..].iter().all(|byte| *byte == 0));
                }
            }
        }
    }

    #[test]
    fn two_blocks_keep_forward_and_depth_records_distinct_after_rebasing() {
        let first = IVec2::new(-3, 5);
        let second = IVec2::new(18, -4);
        let origin = IVec2::new(11, -9);
        let blocks = [
            TerrainViewBlock::from(&block(7, first, 1, &[first])),
            TerrainViewBlock::from(&block(7, second, 1, &[second])),
        ];
        // First cell: SW forward only, SE depth only, NW both, NE neither.
        // Second cell deliberately differs: SW neither, SE both, NW depth, NE forward.
        let forward = covered(&[(first, 0), (first, 2), (second, 1), (second, 3)]);
        let depth = covered(&[(first, 1), (first, 2), (second, 1), (second, 2)]);
        // Seven visible-slot vec4s precede the first block's clip records.
        let prefix = vec![0xa5; 112];
        let mut bytes = prefix.clone();
        for view in &blocks {
            view.append_view_records(Some(7), origin, &forward, &depth, true, &mut bytes);
        }
        assert_eq!(&bytes[..112], prefix.as_slice());
        assert_eq!(bytes.len(), 112 + 640);
        // Fixed byte offsets relative to the clip tail: 0, 160, 320, 480.
        // These correspond to vec4 offsets 0, 10, 20, 30 (absolute bases 7,17,27,37).
        let tail = &bytes[112..];
        let records = [
            record_settings(&tail[0..160]),
            record_settings(&tail[160..320]),
            record_settings(&tail[320..480]),
            record_settings(&tail[480..640]),
        ];
        let expected = [
            [true, false, true, false],
            [false, true, true, false],
            [false, true, false, true],
            [false, true, true, false],
        ];
        for (record_index, record) in records.iter().enumerate() {
            let cell = if record_index < 2 { first } else { second };
            for quadrant in 0..4u8 {
                let (x, z) = quadrant_centre(cell, quadrant, origin);
                assert_eq!(
                    record.clips(x, z),
                    expected[record_index][quadrant as usize],
                    "record {record_index}, cell {cell}, quadrant {quadrant}",
                );
            }
            // Sampling the other block must not accidentally read its mask/origin.
            let other = if record_index < 2 { second } else { first };
            let (x, z) = quadrant_centre(other, 2, origin);
            assert!(
                !record.clips(x, z),
                "record {record_index} leaked across blocks"
            );
        }
        // A non-drawable block's two pass records both fail closed, independent of coverage.
        let mut unavailable = Vec::new();
        blocks[0].append_view_records(Some(7), origin, &forward, &depth, false, &mut unavailable);
        assert_eq!(unavailable.len(), 320);
        for record in [
            record_settings(&unavailable[..160]),
            record_settings(&unavailable[160..]),
        ] {
            assert!(record.clips(1.0e9, -1.0e9));
        }
    }
}
