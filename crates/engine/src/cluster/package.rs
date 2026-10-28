//! Loading a converted cluster package (ADR-0013) into the cluster draw path.
//!
//! A background task certifies the package and retains its exact hashed MCL/MCI bytes.
//! Startup builds the complete scene from that immutable snapshot.
//! The whole scene's metadata stays resident. Camera movement and rebasing never reread geometry.
//! Initial geometry comes from that exact captured snapshot; upload releases shared MCL/tangent
//! storage after queue writes return. A new GPU allocation rereads and verifies released geometry
//! on IoTaskPool against immutable launch digests and typed rows before becoming ready again.
//! Consumed instance-file buffers are released after scene construction; digests and decoded
//! instances remain. Materials use background AssetServer loading.
//!
//! A batch is the instances that draw with one stock material: the same model, glTF material
//! slot and winding. The material is the one the stock path draws that primitive
//! with, loaded through the same glTF label, so the engine's material handler has treated it
//! identically.

use super::{
    assets::{ClusterBlockAsset, ClusterMeshAsset},
    instance_identity::ClusterInstanceIdentity,
    mesh_payload::{MeshExpectation, MeshRehydrateSource, checked_certified_mesh},
    reader::{CertifiedPackage, RecordKey},
    render::{ClusterBatchHandles, ClusterInstance, ClusterScene, SceneBlock, world_from_local},
};
use crate::{
    config::EngineConfig,
    render::WaterReflectionCamera,
    streaming::{ActiveSpace, RenderOrigin},
    world::components::StreamingCamera,
};
use bevy::{
    asset::AssetId,
    ecs::schedule::common_conditions::resource_exists,
    gltf::GltfAssetLabel,
    prelude::*,
    tasks::{IoTaskPool, Task, futures_lite::future},
};
use rusqlite::Connection;
use shared::cluster::hex;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
mod derived_overlay_tests;

/// Registers the converted cluster package draw path.
pub struct ClusterPackagePlugin {
    /// The converted dataset the stock path loads from (`--assets`). Its models are
    /// what the clusters replace.
    pub base: PathBuf,
    /// The certified cluster overlay folder for that dataset: `clusters.db` with the
    /// cluster tables, `clusters/` payloads and `overlay.json`.
    pub overlay: PathBuf,
    /// Register terrain materials and load optional terrain metadata.
    pub terrain: bool,
    /// Where the camera starts, in Creation coordinates (the space of a block's bounds, z up): the
    /// blocks reach the GPU nearest first around it. `None` keeps the package's own order.
    pub priority_point: Option<Vec3>,
}

#[derive(Resource)]
struct PackageSource {
    base: PathBuf,
    overlay: PathBuf,
    terrain: bool,
    priority_point: Option<Vec3>,
}

/// Retains authoritative validation and its dataset lock. This resource does
/// not establish render readiness, which is decided separately for each view.
#[derive(Resource)]
pub(crate) struct ValidatedPackage(pub Arc<CertifiedPackage>);

#[derive(Resource)]
struct PackageLoadStarted;

fn package_load_pending(
    validated: Option<Res<ValidatedPackage>>,
    started: Option<Res<PackageLoadStarted>>,
) -> bool {
    validated.is_some() && started.is_none()
}

/// Package certification status only; `Ready` never means GPU draw-ready.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackageValidationStatus {
    Pending,
    Ready,
    Failed(String),
}

#[derive(Resource)]
struct PendingPackageValidation(Task<Result<CertifiedPackage, String>>);

/// Every tangent worker remains owned until it joins. A failed worker must stop
/// startup rather than leaving a block permanently unready.
#[derive(Resource, Default)]
struct PendingTangents(
    Vec<std::thread::JoinHandle<Result<(), String>>>,
    Arc<std::sync::atomic::AtomicBool>,
);

type TangentBlocks = (
    Vec<ClusterMeshAsset>,
    Vec<SceneBlock>,
    std::sync::atomic::AtomicUsize,
    Arc<std::sync::atomic::AtomicU64>,
);

fn prepare_tangent_blocks(
    prepared: &TangentBlocks,
    terrain_only: &std::collections::HashSet<u32>,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    let (meshes, blocks, next, ready_revision) = prepared;
    loop {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(block) = blocks.get(index) else {
            break;
        };
        for &mesh in &block.meshes {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(());
            }
            let result = if terrain_only.contains(&mesh) {
                meshes[mesh as usize].prepare_unused_tangent_slots()
            } else {
                meshes[mesh as usize].prepare_tangents()
            };
            if let Err(reason) = result {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
                return Err(reason);
            }
        }
        block
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        ready_revision.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
    Ok(())
}

impl Drop for PendingTangents {
    fn drop(&mut self) {
        self.1.store(true, std::sync::atomic::Ordering::Release);
        for worker in self.0.drain(..) {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => error!(%reason, "cluster tangent worker failed during shutdown"),
                Err(_) => error!("cluster tangent worker panicked during shutdown"),
            }
        }
    }
}

/// Apply every asynchronous package failure through the same terminal owner.
/// Deferred worker commands and render receipts may arrive in the same frame.
pub(super) fn refuse_package(world: &mut World, reason: String) {
    if matches!(
        world.get_resource::<PackageValidationStatus>(),
        Some(PackageValidationStatus::Failed(_))
    ) {
        return;
    }
    error!(%reason, "cluster package failed; stopping before world readiness");
    world.insert_resource(PackageValidationStatus::Failed(reason));
    let _ = world.write_message(AppExit::error());
}

fn poll_tangent_workers(pending: Option<ResMut<PendingTangents>>, mut commands: Commands) {
    let Some(mut pending) = pending else { return };
    let mut index = 0;
    while index < pending.0.len() {
        if !pending.0[index].is_finished() {
            index += 1;
            continue;
        }
        let worker = pending.0.swap_remove(index);
        let result = worker
            .join()
            .unwrap_or_else(|_| Err("cluster tangent worker panicked".into()));
        if let Err(reason) = result {
            pending.1.store(true, std::sync::atomic::Ordering::Release);
            commands.queue(move |world: &mut World| refuse_package(world, reason));
        }
    }
    if pending.0.is_empty() {
        commands.remove_resource::<PendingTangents>();
    }
}

// Speculative assets remain private until certification passes. Only explicit all-batch
// preload may request them: ordinary lazy loading keeps its view-based priority and residency.
#[derive(Resource)]
struct PendingModelPrefetch {
    incoming: crossbeam_channel::Receiver<ModelPrefetch>,
    remaining: std::collections::VecDeque<String>,
    scenes: HashMap<String, Handle<WorldAsset>>,
    in_flight: Vec<Handle<WorldAsset>>,
}

impl PendingModelPrefetch {
    fn new(incoming: crossbeam_channel::Receiver<ModelPrefetch>) -> Self {
        Self {
            incoming,
            remaining: default(),
            scenes: default(),
            in_flight: default(),
        }
    }

    fn receive(&mut self) {
        if let Ok(prefetch) = self.incoming.try_recv() {
            self.remaining.extend(prefetch.remaining);
            self.scenes.extend(prefetch.scenes);
            self.in_flight.extend(prefetch.in_flight);
        }
    }
}

struct ModelPrefetch {
    remaining: std::collections::VecDeque<String>,
    scenes: HashMap<String, Handle<WorldAsset>>,
    in_flight: Vec<Handle<WorldAsset>>,
}

fn start_model_prefetch(
    mut paths: Vec<String>,
    preload_all: bool,
    assets: &AssetServer,
) -> ModelPrefetch {
    if !preload_all {
        paths.clear();
    }
    let mut prefetch = ModelPrefetch {
        remaining: paths.into(),
        scenes: default(),
        in_flight: default(),
    };
    // Dispatch on the certification worker: Startup can block the first Update until after
    // certification finishes. AssetServer starts background reads/decoding without that Update.
    while prefetch.in_flight.len() < MAX_LOADING {
        let Some(path) = prefetch.remaining.pop_front() else {
            break;
        };
        let handle = assets.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
        prefetch.in_flight.push(handle.clone());
        prefetch.scenes.insert(path, handle);
    }
    info!(
        models = prefetch.scenes.len(),
        queued = prefetch.remaining.len(),
        "cluster batch model prefetch started; certification remains pending"
    );
    prefetch
}

#[derive(Resource, Default)]
struct PrefetchedBatchModels(HashMap<String, Handle<WorldAsset>>, Vec<Handle<WorldAsset>>);

fn prefetch_batch_paths(
    connection: &Connection,
    published: &[(String, String)],
) -> rusqlite::Result<Vec<String>> {
    let published: std::collections::HashSet<_> =
        published.iter().map(|(path, _)| path.as_str()).collect();
    let mut statement =
        connection.prepare("SELECT DISTINCT model_path FROM cluster_meshes ORDER BY model_path")?;
    let paths = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(paths
        .into_iter()
        .filter(|path| {
            // Published paths already passed reader::published_files' relative-path checks. A
            // malformed material claim cannot turn speculative decoding into an arbitrary read.
            let lower = path.to_ascii_lowercase();
            published.contains(path.as_str())
                && lower.starts_with("meshes/")
                && lower.ends_with(".glb")
        })
        .collect())
}

fn prefetch_pending_models(
    pending: Option<ResMut<PendingModelPrefetch>>,
    assets: Res<AssetServer>,
) {
    let Some(mut pending) = pending else {
        return;
    };
    pending.receive();
    pending
        .scenes
        .retain(|_, handle| !asset_failed(&assets, handle.id()));
    pending.in_flight.retain(|handle| {
        !assets.is_loaded_with_dependencies(handle.id()) && !asset_failed(&assets, handle.id())
    });
    while pending.in_flight.len() < MAX_LOADING {
        let Some(path) = pending.remaining.pop_front() else {
            break;
        };
        let handle = assets.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
        pending.in_flight.push(handle.clone());
        pending.scenes.insert(path, handle);
    }
}

/// The overlay-to-base binding: the overlay must have been built for exactly this conversion of
/// the dataset the stock path loads from. Refuses, naming the rebuild command, an overlay of
/// another base or another conversion of it, and an old full-copy dataset.
#[cfg(test)]
fn checked_package_database(base: &Path, overlay: &Path) -> Result<(), String> {
    shared::cluster_overlay::checked_overlay(base, overlay)
        .map(|_| ())
        .map_err(|error| format!("{error:#}"))
}

fn begin_package_validation(
    mut commands: Commands,
    source: Res<PackageSource>,
    preload: Option<Res<ClusterBatchPreload>>,
    assets: Res<AssetServer>,
) {
    let base = source.base.clone();
    let overlay = source.overlay.clone();
    let terrain = source.terrain;
    let preload_all = preload.is_some();
    let assets = assets.clone();
    let (send, receive) = crossbeam_channel::bounded(1);
    commands.insert_resource(PendingModelPrefetch::new(receive));
    commands.insert_resource(PendingPackageValidation(IoTaskPool::get().spawn(
        async move {
            CertifiedPackage::open_with_prefetch(&base, &overlay, terrain, |connection, published| {
                if !preload_all {
                    return;
                }
                match prefetch_batch_paths(connection, published) {
                    Ok(paths) => {
                        let _ = send.send(start_model_prefetch(paths, preload_all, &assets));
                    }
                    // This is only speculative work. The original validation and loader queries
                    // still run and retain their existing failure policy and refusal verdict.
                    Err(error) => {
                        warn!(%error, "cluster batch prefetch unavailable; certification continues")
                    }
                }
            })
            .map_err(|error| format!("{error:#}"))
        },
    )));
}

fn poll_package_validation(
    mut commands: Commands,
    pending: Option<ResMut<PendingPackageValidation>>,
    prefetched: Option<ResMut<PendingModelPrefetch>>,
    assets: Option<Res<AssetServer>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(mut pending) = pending else {
        return;
    };
    let Some(result) = future::block_on(future::poll_once(&mut pending.0)) else {
        return;
    };
    commands.remove_resource::<PendingPackageValidation>();
    commands.remove_resource::<PendingModelPrefetch>();
    match result {
        Ok(package) => {
            if let Some(mut prefetched) = prefetched {
                // The worker can send after this frame's prefetch system ran, then finish
                // before this poll. Task completion orders the send before this final drain.
                prefetched.receive();
                // A late worker send can contain a failure that the pending system has not
                // seen. Failed speculation must leave the normal loader free to retry.
                if let Some(assets) = &assets {
                    prefetched
                        .scenes
                        .retain(|_, handle| !asset_failed(assets, handle.id()));
                    prefetched
                        .in_flight
                        .retain(|handle| !asset_failed(assets, handle.id()));
                }
                commands.insert_resource(PrefetchedBatchModels(
                    std::mem::take(&mut prefetched.scenes),
                    std::mem::take(&mut prefetched.in_flight),
                ));
            }
            commands.insert_resource(ValidatedPackage(Arc::new(package)));
            commands.insert_resource(PackageValidationStatus::Ready);
        }
        Err(reason) => {
            // An explicitly requested package that fails authoritative validation
            // is not a successful stock-only cluster run. Match the dataset policy
            // for a declared extension that its reader cannot safely consume.
            error!(%reason, "requested cluster package failed authoritative validation");
            commands.insert_resource(PackageValidationStatus::Failed(reason));
            exit.write(AppExit::error());
        }
    }
}

impl Plugin for ClusterPackagePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(super::source_identity::SourceIdentityPlugin);
        if self.terrain {
            app.add_plugins(super::terrain::ClusterTerrainPlugin);
        }
        app.add_plugins(super::render::ClusterRenderPlugin)
            .insert_resource(PackageSource {
                base: self.base.clone(),
                overlay: self.overlay.clone(),
                terrain: self.terrain,
                priority_point: self.priority_point,
            })
            .insert_resource(PackageValidationStatus::Pending)
            .add_systems(Startup, begin_package_validation)
            .add_systems(
                Update,
                (
                    prefetch_pending_models,
                    poll_package_validation.after(prefetch_pending_models),
                    load_cluster_package
                        .after(poll_package_validation)
                        .run_if(package_load_pending),
                    poll_tangent_workers.after(load_cluster_package),
                    load_batches_in_view
                        .after(load_cluster_package)
                        .run_if(resource_exists::<LazyBatches>),
                    // After the loader, which sets the pending count this adds to.
                    count_blocks_not_resident.after(load_batches_in_view),
                ),
            );
    }
}

/// One `cluster_meshes` row.
struct MeshRow {
    expected: MeshExpectation,
    payload_path: String,
    model_path: String,
    material_index: Option<u32>,
}

/// What distinguishes the materials batches draw with.
#[derive(Clone, PartialEq, Eq, Hash)]
struct BatchKey {
    model_path: String,
    material_index: Option<u32>,
    /// Mirrored instances use the glTF loader's scale-inverted variant of the material.
    scale_inverted: bool,
}

fn hash_column(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<[u8; 32]> {
    let value: String = row.get(column)?;
    shared::cluster::parse_hash(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "cluster mesh row has an invalid SHA-256",
            )),
        )
    })
}

fn read_rows(connection: &Connection) -> Result<HashMap<(String, u32), MeshRow>, String> {
    let mut statement = connection
        .prepare(
            "SELECT mesh_hash, primitive_index, payload_path, model_path, material_index,
                    content_hash, payload_bytes, vertex_count, cluster_count FROM cluster_meshes",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, u32>(1)?),
                MeshRow {
                    expected: MeshExpectation {
                        mesh_hash: hash_column(row, 0)?,
                        primitive_index: row.get(1)?,
                        content_hash: hash_column(row, 5)?,
                        payload_bytes: row.get(6)?,
                        vertex_count: row.get(7)?,
                        cluster_count: row.get(8)?,
                    },
                    payload_path: row.get(2)?,
                    model_path: row.get(3)?,
                    material_index: row.get(4)?,
                },
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<HashMap<_, _>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(rows)
}

/// Domain comes from the owning SQL table, never a missing reference mapping.
#[derive(Clone, Copy)]
enum ChunkDomain {
    Object,
    Terrain(u32),
}

/// Material identity is a property of this record's explicit domain, not its
/// shared geometry hash. Object source rows remain authoritative and immutable.
fn record_batch_key(
    row: &MeshRow,
    domain: ChunkDomain,
    blocks: &[super::terrain::TerrainBlock],
    scale_inverted: bool,
) -> Result<BatchKey, String> {
    let (model_path, material_index) = match domain {
        ChunkDomain::Object => (row.model_path.clone(), row.material_index),
        ChunkDomain::Terrain(index) => {
            let block = blocks
                .get(index as usize)
                .ok_or("terrain block index out of range")?;
            if block.mesh_hash != hex(&row.expected.mesh_hash)
                || block.primitive_index != row.expected.primitive_index
            {
                return Err("terrain record geometry differs from its explicit block".into());
            }
            // Package-local block identity keeps equal MCL bytes with distinct
            // atlases/worldspaces separate, even if their meshes remain pooled.
            (format!("cluster-terrain:{index}"), None)
        }
    };
    Ok(BatchKey {
        model_path,
        material_index,
        scale_inverted,
    })
}

fn bind_terrain_batch(
    blocks: &mut [super::terrain::TerrainBlock],
    index: u32,
    batch: u32,
    handles: &mut ClusterBatchHandles,
) -> Result<(), String> {
    let block = blocks
        .get_mut(index as usize)
        .ok_or("terrain block index out of range")?;
    if block.batch.is_some_and(|previous| previous != batch) {
        return Err("terrain block resolved to inconsistent material batches".into());
    }
    handles.set(batch, block.material.clone());
    block.batch = Some(batch);
    Ok(())
}

struct ChunkSource {
    path: String,
    domain: ChunkDomain,
}

fn read_chunk_paths(connection: &Connection) -> Result<Vec<ChunkSource>, String> {
    let mut statement = connection
        .prepare(
            "SELECT payload_path FROM cluster_chunks ORDER BY worldspace_id, anchor_y, anchor_x",
        )
        .map_err(|error| error.to_string())?;
    statement
        .query_map([], |row| {
            Ok(ChunkSource {
                path: row.get(0)?,
                domain: ChunkDomain::Object,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

/// The asset path of the `StandardMaterial` the stock path draws a glTF material slot with: the
/// stock PBR handler publishes it as `<material label>/std`, and the engine's native handler
/// publishes `<material label>/nif` for the materials that need Skyrim's own conventions.
fn standard_material_path(model: &str, slot: u32, scale_inverted: bool, native: bool) -> String {
    shared::cluster_material::material_asset_path(model, slot, scale_inverted, native)
}

/// Read checked native-material slot metadata during covered package startup.
/// An unreadable model is a refusal, never permission to select another material path.
fn native_materials(path: &Path) -> Result<Vec<bool>, String> {
    use std::io::Read;
    let read = || -> Result<Vec<bool>, String> {
        let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut header = [0u8; 20];
        file.read_exact(&mut header)
            .map_err(|error| error.to_string())?;
        let total =
            u32::from_le_bytes(header[8..12].try_into().map_err(|_| "invalid GLB length")?) as u64;
        let json_length = u32::from_le_bytes(
            header[12..16]
                .try_into()
                .map_err(|_| "invalid GLB JSON length")?,
        ) as usize;
        if &header[..4] != b"glTF"
            || header[4..8] != 2u32.to_le_bytes()
            || header[16..20] != 0x4e4f534au32.to_le_bytes()
            || file.metadata().map_err(|error| error.to_string())?.len() != total
            || 20u64 + json_length as u64 > total
        {
            return Err("invalid GLB header or JSON chunk bounds".into());
        }
        let mut document = Vec::with_capacity(20 + json_length);
        document.extend_from_slice(&header);
        document.resize(20 + json_length, 0);
        file.read_exact(&mut document[20..])
            .map_err(|error| error.to_string())?;
        let json_document_length = (document.len() as u32).to_le_bytes();
        document[8..12].copy_from_slice(&json_document_length);
        let gltf =
            bevy::gltf::gltf::Gltf::from_slice(&document).map_err(|error| error.to_string())?;
        Ok(gltf
            .materials()
            .map(|material| crate::nif_material::needs_native_material(&material))
            .collect())
    };
    read().map_err(|error| format!("native material metadata {}: {error}", path.display()))
}

/// A payload reused by any object retains the ordinary tangent-generation path.
pub(crate) fn terrain_only_meshes(instances: &[ClusterInstance]) -> std::collections::HashSet<u32> {
    let mut terrain = std::collections::HashSet::new();
    let mut objects = std::collections::HashSet::new();
    for instance in instances {
        match instance.identity {
            ClusterInstanceIdentity::Terrain { .. } => {
                terrain.insert(instance.mesh);
            }
            ClusterInstanceIdentity::Object(_) | ClusterInstanceIdentity::Fixture => {
                objects.insert(instance.mesh);
            }
        }
    }
    terrain.retain(|mesh| !objects.contains(mesh));
    terrain
}

type PackageLoadGate<'w, 's> = (
    Res<'w, PackageSource>,
    Option<Res<'w, ValidatedPackage>>,
    Local<'s, bool>,
    Option<ResMut<'w, PrefetchedBatchModels>>,
);

fn stop_unusable_overlay(
    exit: &mut MessageWriter<AppExit>,
    package: &CertifiedPackage,
    reason: impl std::fmt::Display,
) {
    let envelope = package.envelope();
    error!(
        %reason,
        rebuild = %envelope.metadata.rebuild_hint(&envelope.root),
        "requested cluster overlay payload is unusable; stopping before publishing the scene"
    );
    exit.write(AppExit::error());
}

fn load_cluster_package(
    mut commands: Commands,
    gate: PackageLoadGate<'_, '_>,
    mut handles: ResMut<ClusterBatchHandles>,
    mut images: ResMut<Assets<Image>>,
    mut terrain_materials: Option<ResMut<Assets<super::terrain::ClusterTerrainMaterial>>>,
    mut exit: MessageWriter<AppExit>,
) {
    let (source, validated, mut loaded, mut prefetched) = gate;
    if *loaded {
        return;
    }
    let Some(validated) = validated else {
        return;
    };
    *loaded = true;
    commands.insert_resource(PackageLoadStarted);
    let (prefetched_scenes, prefetch_in_flight) = prefetched
        .as_deref_mut()
        .map(|models| (std::mem::take(&mut models.0), std::mem::take(&mut models.1)))
        .unwrap_or_default();
    commands.remove_resource::<PrefetchedBatchModels>();
    // Core files (models, textures, the cell cache) live in the base; payloads, manifests and the
    // cluster tables live in the overlay.
    let base_root = &validated.0.envelope().root;
    let connection = match validated.0.envelope().connection() {
        Ok(connection) => connection,
        // The clusters were asked for and certified: an overlay that no longer opens bound to its
        // base (changed after certification) stops the run, it never draws without them.
        Err(error) => {
            error!("requested cluster overlay unreadable: {error:#}");
            exit.write(AppExit::error());
            return;
        }
    };
    let rows = match read_rows(&connection) {
        Ok(rows) => rows,
        Err(error) => {
            error!("requested cluster overlay unreadable: {error}");
            exit.write(AppExit::error());
            return;
        }
    };
    let mut chunk_paths = match read_chunk_paths(&connection) {
        Ok(paths) => paths,
        Err(error) => {
            error!("requested cluster overlay chunks unreadable: {error}");
            exit.write(AppExit::error());
            return;
        }
    };
    let mut terrain = if source.terrain {
        let Some(materials) = terrain_materials.as_deref_mut() else {
            error!("cluster terrain material plugin is missing");
            exit.write(AppExit::error());
            return;
        };
        match super::terrain::ClusterTerrain::load_certified(
            validated.0.envelope(),
            &mut images,
            materials,
        ) {
            Ok(terrain) => Some(terrain),
            Err(error) => {
                error!("cluster terrain package refused: {error}");
                exit.write(AppExit::error());
                return;
            }
        }
    } else {
        None
    };
    if let Some(terrain) = &mut terrain {
        if let Some(materials) = terrain_materials.as_deref_mut() {
            terrain.set_near_coverage(
                None,
                &std::collections::HashSet::new(),
                IVec2::ZERO,
                &super::terrain::TerrainDrawReadiness::default(),
                materials,
            );
        }
        chunk_paths.extend(
            terrain
                .blocks
                .iter()
                .enumerate()
                .map(|(index, block)| ChunkSource {
                    path: block.instance_path.clone(),
                    domain: ChunkDomain::Terrain(index as u32),
                }),
        );
    }
    let identity = validated.0.envelope().builder_identity.clone();

    let mut mesh_index: HashMap<(String, u32), u32> = HashMap::new();
    let mut scene_meshes: Vec<ClusterMeshAsset> = Vec::new();
    let mut batch_index: HashMap<(BatchKey, u32), u32> = HashMap::new();
    let mut batch_keys: Vec<(BatchKey, u32)> = Vec::new();
    // Where each instance is, for loading its batch's material when it comes into view.
    let mut instance_spheres: Vec<(Vec3, f32)> = Vec::new();
    let mut instances: Vec<ClusterInstance> = Vec::new();
    // Which block each instance came from, and every block's box.
    let mut instance_block: Vec<u32> = Vec::new();
    let mut block_bounds: Vec<([f32; 3], [f32; 3])> = Vec::new();
    for chunk_source in &chunk_paths {
        let chunk = &chunk_source.path;
        let payload = match validated.0.envelope().payload(chunk) {
            Ok(payload) => payload,
            Err(error) => {
                stop_unusable_overlay(
                    &mut exit,
                    &validated.0,
                    format_args!("cluster block {chunk} unreadable: {error}"),
                );
                return;
            }
        };
        let block = match ClusterBlockAsset::from_bytes(payload.bytes()) {
            Ok(block) => block,
            Err(error) => {
                stop_unusable_overlay(
                    &mut exit,
                    &validated.0,
                    format_args!("cluster block {chunk} invalid: {error}"),
                );
                return;
            }
        };
        if hex(&block.builder_identity) != identity {
            stop_unusable_overlay(
                &mut exit,
                &validated.0,
                format_args!(
                    "cluster block {chunk} has another builder identity; expected {identity}"
                ),
            );
            return;
        }
        let block_number = block_bounds.len() as u32;
        block_bounds.push((block.bounds_min, block.bounds_max));
        // The block's mesh table, resolved once per block.
        let mut block_meshes: Vec<(u32, &MeshRow)> = Vec::new();
        for reference in &block.meshes {
            let key = (hex(&reference.mesh_hash), reference.primitive_index);
            let Some(row) = rows.get(&key) else {
                stop_unusable_overlay(
                    &mut exit,
                    &validated.0,
                    format_args!(
                        "cluster block {chunk} references missing mesh {} primitive {}",
                        key.0, key.1
                    ),
                );
                return;
            };
            let index = match mesh_index.get(&key) {
                Some(index) => *index,
                None => {
                    let loaded = validated
                        .0
                        .envelope()
                        .payload(&row.payload_path)
                        .map_err(|error| error.to_string())
                        .and_then(|payload| {
                            let builder = shared::cluster::parse_hash(&identity)
                                .ok_or("invalid package builder identity")?;
                            let mut mesh =
                                checked_certified_mesh(&payload, builder, &row.expected)?;
                            mesh.set_rehydrate_source(MeshRehydrateSource::new(
                                validated.0.clone(),
                                row.payload_path.clone(),
                                builder,
                                row.expected,
                            )?);
                            Ok(mesh)
                        });
                    match loaded {
                        Ok(mesh) => {
                            let index = scene_meshes.len() as u32;
                            scene_meshes.push(mesh);
                            mesh_index.insert(key.clone(), index);
                            index
                        }
                        Err(error) => {
                            stop_unusable_overlay(
                                &mut exit,
                                &validated.0,
                                format_args!("cluster mesh {} unusable: {error}", row.payload_path),
                            );
                            return;
                        }
                    }
                }
            };
            block_meshes.push((index, row));
        }
        // Preserve the final writer order as the stable record identity.
        // Neither scene reorder nor later batch sorting may renumber this key.
        for (original_ordinal, record) in block.instances.iter().enumerate() {
            let instance_identity = match chunk_source.domain {
                ChunkDomain::Object => ClusterInstanceIdentity::Object(RecordKey {
                    worldspace: block.worldspace_id,
                    tier: block.tier,
                    anchor: [block.anchor_x, block.anchor_y],
                    // read_mci represents the record count as u32.
                    ordinal: original_ordinal as u32,
                }),
                ChunkDomain::Terrain(index) => ClusterInstanceIdentity::Terrain {
                    block: index,
                    worldspace: block.worldspace_id,
                    tier: block.tier,
                    anchor: [block.anchor_x, block.anchor_y],
                },
            };
            let Some((mesh, row)) = block_meshes.get(record.mesh as usize) else {
                stop_unusable_overlay(
                    &mut exit,
                    &validated.0,
                    format_args!(
                        "cluster block {chunk} record {original_ordinal} references missing mesh index {}",
                        record.mesh
                    ),
                );
                return;
            };
            let matrix = world_from_local(record);
            let terrain_blocks = terrain
                .as_ref()
                .map_or(&[][..], |terrain| terrain.blocks.as_slice());
            let key = match record_batch_key(
                row,
                chunk_source.domain,
                terrain_blocks,
                matrix.determinant() < 0.0,
            ) {
                Ok(key) => key,
                Err(reason) => {
                    error!(%reason, "cluster terrain material identity refused");
                    exit.write(AppExit::error());
                    return;
                }
            };
            // Actual loader-generated attributes belong to a source occurrence, not
            // to a material or the MCL authored-attribute flags. Keep that identity
            // so later material edits cannot merge different stock layouts.
            let primitive = row.expected.primitive_index;
            let batch = *batch_index
                .entry((key.clone(), primitive))
                .or_insert_with(|| {
                    batch_keys.push((key, primitive));
                    (batch_keys.len() - 1) as u32
                });
            if let ChunkDomain::Terrain(index) = chunk_source.domain {
                let Some(terrain) = terrain.as_mut() else {
                    error!("terrain instance has no terrain metadata");
                    exit.write(AppExit::error());
                    return;
                };
                if let Err(reason) =
                    bind_terrain_batch(&mut terrain.blocks, index, batch, &mut handles)
                {
                    error!(%reason, "cluster terrain material binding refused");
                    exit.write(AppExit::error());
                    return;
                }
            }
            let bounds = scene_meshes[*mesh as usize].bounds;
            let scale = matrix
                .x_axis
                .truncate()
                .length()
                .max(matrix.y_axis.truncate().length())
                .max(matrix.z_axis.truncate().length());
            instance_spheres.push((
                matrix.transform_point3(Vec3::new(bounds[0], bounds[1], bounds[2])),
                bounds[3] * scale,
            ));
            instances.push(ClusterInstance {
                identity: instance_identity,
                mesh: *mesh,
                batch,
                world_from_local: matrix,
                source_worldspace: record.source_worldspace_id,
            });
            instance_block.push(block_number);
        }
    }
    let mut batch_spheres: Vec<Vec<LazySphere>> = vec![Vec::new(); batch_keys.len()];
    for (instance, sphere) in instances.iter().zip(&instance_spheres) {
        batch_spheres[instance.batch as usize].push(LazySphere {
            center: sphere.0,
            radius: sphere.1,
            world: lazy_instance_world(&instance.identity, Some(validated.0.as_ref())),
        });
    }
    if instances.is_empty() {
        // A certified package may have no eligible instances. Still publish its
        // explicit empty scene so initial automation can distinguish it from loading.
        warn!("cluster package holds no drawable instances, drawing none");
    }

    // Freeze slot metadata during startup too. The lazy material-request path
    // performs no filesystem reads during steady rendering.
    let mut native_by_model = HashMap::new();
    for (key, _) in &batch_keys {
        let Some(slot) = key.material_index else {
            continue;
        };
        if key.model_path.starts_with("cluster-terrain:") {
            continue;
        }
        let native = match native_by_model.entry(key.model_path.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                match native_materials(&base_root.join(&key.model_path)) {
                    Ok(native) => entry.insert(native),
                    Err(error) => {
                        stop_unusable_overlay(&mut exit, &validated.0, error);
                        return;
                    }
                }
            }
        };
        if native.get(slot as usize).is_none() {
            stop_unusable_overlay(
                &mut exit,
                &validated.0,
                format_args!(
                    "model {} has no certified material slot {slot}",
                    key.model_path
                ),
            );
            return;
        }
    }
    // A batch entity is cheap and exists from the start; its material is requested only once an
    // instance of the batch comes into view (`load_batches_in_view`), so loading follows what the
    // camera sees and not the size of the package.
    let layout_scope = next_layout_scope();
    commands.insert_resource(LazyBatches {
        layout_scope,
        batches: batch_keys
            .iter()
            .zip(batch_spheres)
            .map(|((key, primitive), spheres)| LazyBatch {
                key: key.clone(),
                primitive: *primitive,
                bounds: enclosing_sphere(
                    &spheres
                        .iter()
                        .map(|sphere| (sphere.center, sphere.radius))
                        .collect::<Vec<_>>(),
                ),
                spheres,
                scene: None,
                source_reload_from: None,
                loading: None,
                requested: key.model_path.starts_with("cluster-terrain:"),
                requested_at: 0.0,
                reported: false,
            })
            .collect(),
        native_by_model,
        scenes: prefetched_scenes,
        retried_sources: default(),
        prefetch_in_flight,
        unrequested: (0..batch_keys.len())
            .filter(|&index| {
                !batch_keys[index]
                    .0
                    .model_path
                    .starts_with("cluster-terrain:")
            })
            .collect(),
        in_flight: Vec::new(),
        failed: Vec::new(),

        scenes_revision: 0,
        failed_revision: 0,
    });
    info!(
        meshes = scene_meshes.len(),
        instances = instances.len(),
        batches = batch_keys.len(),
        blocks = chunk_paths.len(),
        "cluster package loaded"
    );
    let terrain_view_blocks = terrain.as_ref().map_or_else(Vec::new, |terrain| {
        terrain
            .blocks
            .iter()
            .map(super::terrain::TerrainViewBlock::from)
            .collect()
    });
    if let Some(terrain) = terrain {
        if terrain.blocks.iter().any(|block| block.batch.is_none()) {
            error!("cluster terrain has a block with no valid drawable payload");
            exit.write(AppExit::error());
            return;
        }
        commands.insert_resource(TerrainBatchIds(
            terrain
                .blocks
                .iter()
                .filter_map(|block| block.batch)
                .collect(),
        ));
        commands.insert_resource(terrain);
    }
    // The scene reaches the GPU a block at a time, nearest the camera's start first. Each block is a
    // run of the scene's instances, so the instances are laid out block by block in that order.
    let mut per_block: Vec<Vec<usize>> = vec![Vec::new(); block_bounds.len()];
    for (index, block) in instance_block.iter().enumerate() {
        per_block[*block as usize].push(index);
    }
    let mut order: Vec<usize> = (0..block_bounds.len())
        .filter(|&block| !per_block[block].is_empty())
        .collect();
    if let Some(point) = source.priority_point {
        let distance = |block: usize| {
            let (low, high) = block_bounds[block];
            distance_to_box(point.to_array(), low, high)
        };
        order.sort_by(|a, b| distance(*a).total_cmp(&distance(*b)));
    }
    let mut ordered: Vec<ClusterInstance> = Vec::with_capacity(instances.len());
    let mut scene_blocks: Vec<SceneBlock> = Vec::with_capacity(order.len());
    for block in order {
        let first = ordered.len();
        let mut mesh_ids: Vec<u32> = Vec::new();
        for &index in &per_block[block] {
            ordered.push(instances[index]);
            mesh_ids.push(instances[index].mesh);
        }
        mesh_ids.sort_unstable();
        mesh_ids.dedup();
        scene_blocks.push(SceneBlock {
            first_instance: first as u32,
            instance_count: (ordered.len() - first) as u32,
            meshes: mesh_ids,
            ready: Arc::default(),
            resident: Arc::default(),
        });
    }
    let instances = ordered;
    let inventory = match super::instance_inventory::InstanceInventory::build(
        validated.0.clone(),
        layout_scope,
        &instances,
        batch_keys.len(),
    ) {
        Ok(inventory) => Arc::new(inventory),
        Err(reason) => {
            error!(%reason, "cluster instance inventory could not be certified");
            exit.write(AppExit::error());
            return;
        }
    };
    if let Some(point) = source.priority_point {
        // The first blocks, and how far from the start they are: the order is the point.
        let nearest: Vec<String> = scene_blocks
            .iter()
            .take(3)
            .map(|block| {
                let instance = &instances[block.first_instance as usize];
                format!(
                    "{} instances, first at {:?}",
                    block.instance_count,
                    instance.world_from_local.w_axis.truncate()
                )
            })
            .collect();
        info!(start = ?point, ?nearest, "cluster blocks are ordered nearest the start first");
    }
    // Terrain's colour-only atlas never reads tangents. Fill its unused GPU slots without
    // generating geometry derivatives; shared object payloads keep ordinary tangent generation.
    let terrain_only = Arc::new(terrain_only_meshes(&instances));
    info!(
        meshes = terrain_only.len(),
        "cluster terrain tangent generation skipped (unused slots)"
    );
    // Prepare object tangents off the main/render threads. The retained handles
    // report worker panics through the startup error path and join on shutdown.
    let threads = (std::thread::available_parallelism().map_or(2, usize::from) / 2).max(1);
    let ready_revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let prepared = Arc::new((
        scene_meshes.clone(),
        scene_blocks.clone(),
        std::sync::atomic::AtomicUsize::new(0),
        ready_revision.clone(),
    ));
    let mut workers = PendingTangents::default();
    for _ in 0..threads {
        let prepared = prepared.clone();
        let terrain_only = terrain_only.clone();
        let cancelled = workers.1.clone();
        let worker = std::thread::Builder::new()
            .name("cluster-tangents".into())
            .spawn(move || prepare_tangent_blocks(&prepared, &terrain_only, &cancelled));
        match worker {
            Ok(worker) => workers.0.push(worker),
            Err(error) => {
                stop_unusable_overlay(
                    &mut exit,
                    &validated.0,
                    format_args!("cannot start cluster tangent worker: {error}"),
                );
                return;
            }
        }
    }
    if let Err(error) = validated.0.envelope().release_consumed_payload_bytes() {
        stop_unusable_overlay(&mut exit, &validated.0, error);
        return;
    }
    commands.insert_resource(workers);
    commands.insert_resource(ClusterScene {
        ready_revision,
        residency_revision: default(),
        certificate: Some(validated.0.clone()),
        inventory: Some(inventory),
        terrain_blocks: terrain_view_blocks,
        meshes: scene_meshes,
        instances,
        origin_offset: Vec3::ZERO,
        blocks: scene_blocks,
    });
}

/// Holds the world back from being ready while blocks of the scene have not reached the GPU: a
/// capture or a benchmark waits for all of them, so its pictures do not depend on the timing of the
/// stream-in.
#[derive(Default)]
struct BlockResidentCache {
    source: Option<Arc<std::sync::atomic::AtomicU64>>,
    revision: u64,
    pending: usize,
    #[cfg(test)]
    block_checks: usize,
}

impl BlockResidentCache {
    fn pending(&mut self, scene: &ClusterScene, changed: bool) -> usize {
        let revision = scene
            .residency_revision
            .load(std::sync::atomic::Ordering::Acquire);
        if changed
            || self.revision != revision
            || !self
                .source
                .as_ref()
                .is_some_and(|source| Arc::ptr_eq(source, &scene.residency_revision))
        {
            #[cfg(test)]
            {
                self.block_checks += scene.blocks.len();
            }
            self.pending = scene
                .blocks
                .iter()
                .filter(|block| !block.resident.load(std::sync::atomic::Ordering::Acquire))
                .count();
            self.source = Some(scene.residency_revision.clone());
            self.revision = revision;
        }
        self.pending
    }
}

fn count_blocks_not_resident(
    scene: Option<Res<ClusterScene>>,
    validation: Option<Res<PackageValidationStatus>>,
    mut metrics: Option<ResMut<crate::streaming::StreamingMetrics>>,
    mut cache: Local<BlockResidentCache>,
) {
    let Some(metrics) = metrics.as_deref_mut() else {
        return;
    };
    if matches!(
        validation.as_deref(),
        Some(PackageValidationStatus::Pending)
    ) {
        // The loader does not exist yet to reset its counts. Assign rather than
        // accumulate, so every pending frame holds settle with the same marker.
        metrics.pending_cluster_batches = 1;
        return;
    }
    let Some(scene) = scene else {
        return;
    };
    let waiting = cache.pending(&scene, scene.is_changed());
    if waiting != 0 {
        metrics.pending_cluster_batches += waiting;
    }
}

/// Euclidean distance to the nearest point of an axis-aligned package block.
fn distance_to_box(point: [f32; 3], min: [f32; 3], max: [f32; 3]) -> f32 {
    let mut sum = 0.0f32;
    for axis in 0..3 {
        let gap = (min[axis] - point[axis])
            .max(point[axis] - max[axis])
            .max(0.0);
        sum += gap * gap;
    }
    sum.sqrt()
}

/// A sphere that holds every sphere of `spheres` (the middle of their box, reaching the farthest).
fn enclosing_sphere(spheres: &[(Vec3, f32)]) -> (Vec3, f32) {
    let (low, high) = spheres.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(low, high), (center, _)| (low.min(*center), high.max(*center)),
    );
    if spheres.is_empty() {
        return (Vec3::ZERO, 0.0);
    }
    let middle = (low + high) * 0.5;
    let radius = spheres
        .iter()
        .map(|(center, radius)| center.distance(middle) + radius)
        .fold(0.0, f32::max);
    (middle, radius)
}

/// Fixture is a deliberate invented-scene domain, never a fallback for missing authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LazyWorld {
    Placed(u32),
    Fixture,
    Unknown,
}

#[derive(Clone, Copy)]
struct LazySphere {
    center: Vec3,
    radius: f32,
    world: LazyWorld,
}

fn lazy_instance_world(
    identity: &ClusterInstanceIdentity,
    certificate: Option<&CertifiedPackage>,
) -> LazyWorld {
    if matches!(identity, ClusterInstanceIdentity::Fixture) && certificate.is_none() {
        LazyWorld::Fixture
    } else {
        identity
            .placed_worldspace(certificate)
            .map_or(LazyWorld::Unknown, LazyWorld::Placed)
    }
}

/// The streaming camera and its water reflection share the active exterior world.
fn lazy_camera_world(streaming: bool, reflection: bool, active_world: Option<u32>) -> Option<u32> {
    if streaming || reflection {
        active_world
    } else {
        None
    }
}

fn lazy_world_matches(instance: LazyWorld, camera: Option<u32>) -> bool {
    match instance {
        LazyWorld::Placed(world) => camera == Some(world),
        LazyWorld::Fixture => true,
        LazyWorld::Unknown => false,
    }
}

struct LazyView {
    frustum: bevy::camera::primitives::Frustum,
    transform: GlobalTransform,
    world: Option<u32>,
}

impl LazyView {
    fn snapshot(&self) -> (GlobalTransform, [Vec4; 6], Option<u32>) {
        (
            self.transform,
            self.frustum.half_spaces.map(|plane| plane.normal_d()),
            self.world,
        )
    }
}

/// A cross-world sphere is never a candidate, even at identical coordinates.
/// The enclosing sphere is only a conservative fast rejection; each matching instance
/// still receives the existing margin and exact frustum/distance test.
fn lazy_view_distance(
    bounds: (Vec3, f32),
    spheres: &[LazySphere],
    view: &LazyView,
    offset: Vec3,
    margin: f32,
) -> f32 {
    if !view.frustum.intersects_sphere(
        &bevy::camera::primitives::Sphere {
            center: Vec3A::from(bounds.0 + offset),
            radius: bounds.1 + margin,
        },
        false,
    ) {
        return f32::INFINITY;
    }
    spheres
        .iter()
        .filter(|sphere| lazy_world_matches(sphere.world, view.world))
        .filter_map(|sphere| {
            let center = sphere.center + offset;
            view.frustum
                .intersects_sphere(
                    &bevy::camera::primitives::Sphere {
                        center: Vec3A::from(center),
                        radius: sphere.radius + margin,
                    },
                    false,
                )
                .then(|| view.transform.translation().distance(center))
        })
        .fold(f32::INFINITY, f32::min)
}

/// One batch whose material is not requested until it is seen.
struct LazyBatch {
    key: BatchKey,
    primitive: u32,
    /// Absolute spheres retain each instance's certified PLACED world, not its storage block.
    spheres: Vec<LazySphere>,
    /// One sphere around all of `spheres`: a batch whose own is out of the view has no instance in
    /// it, and its instances are not tested one by one.
    bounds: (Vec3, f32),
    /// The model's scene, requested first, by the same path and label the cell loader uses, so
    /// the asset server loads the model's file once for both. Asking for one of the file's
    /// other labels while the cell loader's scene load is in flight is a second load of the
    /// file, and a second load re-instances every drawn copy of the model, losing
    /// the stock-visibility annotations on those copies.
    /// Held only until the file is in; the material is requested then.
    scene: Option<Handle<WorldAsset>>,
    /// One forced retry must publish a new result before old dependency failure is terminal.
    source_reload_from: Option<bevy::ecs::world::WorldId>,
    /// The base material while it is still loading; cleared once it is ready (or has failed).
    loading: Option<Handle<StandardMaterial>>,
    requested: bool,
    /// When the material was requested (seconds since startup), and whether a request that has
    /// taken too long was reported.
    requested_at: f32,
    reported: bool,
}

/// Present when every batch is to be requested during the startup load
/// (all-batch preload), not only those in or near a view.
#[derive(Resource)]
pub(crate) struct ClusterBatchPreload;

#[derive(Resource)]
pub(super) struct LazyBatches {
    pub(super) layout_scope: u64,
    batches: Vec<LazyBatch>,
    native_by_model: HashMap<String, Vec<bool>>,
    /// Every model scene the loader has asked for, kept for the run. The cell loader asks for the
    /// same scenes by the same path; if this side let go of one while the other's load or spawn
    /// was in flight, the asset would be freed and loaded again, which re-instances the stock copy
    /// and loses its cluster-owned primitive visibility annotations.
    scenes: HashMap<String, Handle<WorldAsset>>,
    /// Failed cached dependencies get at most one forced restart per source path.
    retried_sources: std::collections::HashSet<String>,
    // Source I/O begun before certification. Capacity only: never ordinary view demand.
    prefetch_in_flight: Vec<Handle<WorldAsset>>,
    /// The batches whose material has not been requested yet, ascending. Everything the loader
    /// looks for each frame (a batch coming into view, a hole in the picture) is among these or
    /// among `in_flight`, so a frame never walks the batches that are loaded and done.
    unrequested: Vec<usize>,
    /// The batches whose scene or material is being loaded.
    in_flight: Vec<usize>,
    /// Terminal source/material failures remain explicit, separate from pending work.
    failed: Vec<usize>,
    /// Bumped whenever `scenes` gains or loses a model, so a reader of `source_scene` can tell
    /// an unchanged map without looking every batch up (the loader touches this resource every
    /// frame, so its change tick says nothing).
    scenes_revision: u64,
    /// Bumped whenever a batch joins `failed`, for the same reason.
    failed_revision: u64,
}

fn next_layout_scope() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl LazyBatches {
    /// One explicitly mapped source for real-loader extraction tests, without the streaming loader.
    #[cfg(test)]
    pub(crate) fn for_source_test(model_path: &str, scene: Handle<WorldAsset>) -> Self {
        Self {
            layout_scope: next_layout_scope(),
            batches: vec![LazyBatch {
                primitive: 0,
                key: BatchKey {
                    model_path: model_path.to_owned(),
                    material_index: Some(0),
                    scale_inverted: false,
                },
                spheres: Vec::new(),
                bounds: (Vec3::ZERO, 0.0),
                scene: Some(scene.clone()),
                source_reload_from: None,
                loading: None,
                requested: true,
                requested_at: 0.0,
                reported: false,
            }],
            native_by_model: HashMap::new(),
            scenes: HashMap::from([(model_path.to_owned(), scene)]),
            retried_sources: default(),
            prefetch_in_flight: Vec::new(),
            unrequested: Vec::new(),
            in_flight: vec![0],
            failed: Vec::new(),

            scenes_revision: 0,
            failed_revision: 0,
        }
    }

    fn prefetch_only_in_flight(&self) -> usize {
        let ordinary: std::collections::HashSet<_> = self
            .in_flight
            .iter()
            .filter_map(|&index| self.batches[index].scene.as_ref().map(Handle::id))
            .collect();
        self.prefetch_in_flight
            .iter()
            .filter(|handle| !ordinary.contains(&handle.id()))
            .count()
    }

    pub(super) fn source_progress(&self, batch: usize) -> Option<(bool, bool, bool)> {
        let batch = self.batches.get(batch)?;
        Some((
            batch.requested,
            batch.scene.is_some(),
            batch.loading.is_some(),
        ))
    }

    pub(super) fn source_primitive(&self, batch: u32) -> Option<u32> {
        Some(self.batches.get(batch as usize)?.primitive)
    }

    /// The batches whose source or material failed for good; the loader never retries them.
    pub(super) fn failed_batches(&self) -> &[usize] {
        &self.failed
    }

    /// Changes whenever `failed_batches` gains a batch within one batch layout.
    pub(super) fn failed_key(&self) -> (u64, u64) {
        (self.layout_scope, self.failed_revision)
    }

    /// Renderer refusal is a resource failure, never a new ownership decision.
    /// Scope (certificate, inventory and layout) is checked before this callback.
    pub(super) fn refuse_gpu_batch(&mut self, batch: usize) -> bool {
        if batch >= self.batches.len() {
            return false;
        }
        if !self.failed.contains(&batch) {
            self.failed.push(batch);
            self.failed_revision = self.failed_revision.wrapping_add(1);
            self.unrequested.retain(|&index| index != batch);
            self.in_flight.retain(|&index| index != batch);
            self.batches[batch].loading = None;
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn mark_failed_for_test(&mut self, batch: usize) {
        self.failed.push(batch);
        self.failed_revision += 1;
    }

    pub(super) fn is_failed(&self, batch: u32) -> bool {
        self.failed.contains(&(batch as usize))
    }

    /// What every `source_scene` answer depends on: the batch layout and the scene map.
    pub(super) fn source_scenes_key(&self) -> (u64, u64) {
        (self.layout_scope, self.scenes_revision)
    }

    pub(super) fn source_scene(&self, batch: u32) -> Option<&Handle<WorldAsset>> {
        self.scenes
            .get(&self.batches.get(batch as usize)?.key.model_path)
    }
}

/// How far outside the view an instance may be and still have its material requested, in world
/// units: the model needs time to load before the camera turns to it.
const LOAD_MARGIN: f32 = 2048.0;

/// How long a material request may take before it is reported as stalled.
const STALLED_SECONDS: f32 = 20.0;

/// Model files that may be loading at once. Every source is a whole glTF with its textures; asking
/// for all of a landscape's at once starves the engine's own streaming of the same task pool. The
/// limit counts files being read, not batches: a batch whose model is already loaded (or already
/// being loaded for another batch) starts no read, and its material is a label of that model.
const MAX_LOADING: usize = 16;

/// What the last full look at the batches saw, so a frame in which nothing it depends on moved can
/// reuse it: the cameras (their frustum and transform change flags), the render origin offset, and
/// how many batches are unrequested, in flight and loading.
#[derive(Default)]
struct ScanCache {
    valid: bool,
    layout_scope: u64,
    failed_revision: u64,
    failed_visible: usize,
    offset: Vec3,
    counts: (usize, usize, usize, usize),
    // Wanted batches still blocked by speculative I/O capacity.
    waiting: usize,
    /// Each camera's transform and frustum planes as they were: compared by value, because a
    /// harness that re-applies the same pose every frame marks the transform changed without
    /// moving it.
    cameras: Vec<(GlobalTransform, [Vec4; 6], Option<u32>)>,
}

/// Requests the material of the batches that have an instance in or near a camera's view, the
/// nearest first, keeping at most [`MAX_LOADING`] requests in flight.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn load_batches_in_view(
    mut handles: ResMut<ClusterBatchHandles>,
    mut lazy: ResMut<LazyBatches>,
    scene: Option<Res<ClusterScene>>,
    cameras: Query<
        (
            &Camera,
            &bevy::camera::primitives::Frustum,
            &GlobalTransform,
            Has<StreamingCamera>,
            Has<WaterReflectionCamera>,
        ),
        With<Camera3d>,
    >,
    (asset_server, world_assets): (Res<AssetServer>, Res<Assets<WorldAsset>>),
    mut scan: Local<ScanCache>,
    #[cfg(test)] mut scan_work: Option<ResMut<LazyScanWork>>,
    mut standard_materials: ResMut<Assets<StandardMaterial>>,
    mut metrics: Option<ResMut<crate::streaming::StreamingMetrics>>,
    mut exit: MessageWriter<AppExit>,
    time: Res<Time>,
    origin: Option<Res<RenderOrigin>>,
    config: Res<EngineConfig>,
    (active, preload): (Option<Res<ActiveSpace>>, Option<Res<ClusterBatchPreload>>),
) {
    let Some(scene) = scene else {
        return;
    };
    // The same offset the render world places the instances with this frame.
    let offset = scene.origin_offset
        + origin.map_or(Vec3::ZERO, |origin| {
            super::render::render_origin_offset(origin.0)
        });
    let active_world = if active
        .as_ref()
        .is_some_and(|active| active.interior.is_some())
    {
        None
    } else {
        Some(
            active
                .as_ref()
                .and_then(|active| active.worldspace_id)
                .unwrap_or(config.worldspace_id),
        )
    };
    let preload = preload.is_some();
    let views_unchanged = cameras
        .iter()
        .filter(|(camera, ..)| camera.is_active)
        .map(|(_, frustum, transform, streaming, reflection)| {
            (
                *transform,
                frustum.half_spaces.map(|plane| plane.normal_d()),
                lazy_camera_world(streaming, reflection, active_world),
            )
        })
        .eq(scan.cameras.iter().copied());
    if scan.valid
        && scan.layout_scope == lazy.layout_scope
        && scan.failed_revision == lazy.failed_revision
        && scan.offset == offset
        && scan.counts.0 == lazy.unrequested.len()
        && lazy.in_flight.is_empty()
        && lazy.prefetch_in_flight.is_empty()
        && views_unchanged
        && !(preload && !lazy.unrequested.is_empty())
    {
        if let Some(metrics) = metrics.as_deref_mut() {
            if metrics.pending_cluster_batches != scan.waiting {
                metrics.pending_cluster_batches = scan.waiting;
            }
            if metrics.failed_cluster_batches != scan.failed_visible {
                metrics.failed_cluster_batches = scan.failed_visible;
            }
        }
        return;
    }
    #[cfg(test)]
    if let Some(work) = scan_work.as_mut() {
        work.0 += 1;
    }
    let views: Vec<LazyView> = cameras
        .iter()
        .filter(|(camera, ..)| camera.is_active)
        .map(|(_, frustum, transform, streaming, reflection)| LazyView {
            frustum: *frustum,
            transform: *transform,
            world: lazy_camera_world(streaming, reflection, active_world),
        })
        .collect();
    let lazy = &mut *lazy;
    // Requests that have finished (or failed) stop counting against the limit. `loading` counts
    // the batches still waiting (they hold readiness); `loading_sources` the model files being
    // read for them, which is what the limit bounds.
    let mut loading = 0usize;
    let mut loading_sources: std::collections::HashSet<AssetId<WorldAsset>> =
        std::collections::HashSet::new();
    let in_flight = std::mem::take(&mut lazy.in_flight);
    let mut still_in_flight: Vec<usize> = Vec::with_capacity(in_flight.len());
    for index in in_flight {
        let batch = &mut lazy.batches[index];
        // Phase one: the model's file is being loaded under the scene label the cell loader
        // uses. Once it is in (its sub-assets exist), the material label is a lookup, not a
        // second load of the file.
        if let Some(scene) = &batch.scene {
            if let Some(previous) = batch.source_reload_from {
                // glTF reloads construct a new template World. In-place annotations keep its
                // identity, so they cannot acknowledge a retry before its actual publication.
                let published = world_assets
                    .get(scene.id())
                    .is_some_and(|asset| asset.world.id() != previous);
                let failed = matches!(
                    asset_server.load_state(scene.id()),
                    bevy::asset::LoadState::Failed(_)
                );
                if !published && !failed {
                    loading += 1;
                    loading_sources.insert(scene.id());
                    still_in_flight.push(index);
                    continue;
                }
                batch.source_reload_from = None;
            }
            if asset_failed(&asset_server, scene.id()) {
                warn!(model = %batch.key.model_path, "cluster source scene failed; stock replacement remains disabled");
                batch.scene = None;
                lazy.failed.push(index);
                lazy.failed_revision += 1;
                continue;
            }
            if source_asset_ready(
                world_assets.contains(scene.id()),
                asset_server.is_loaded_with_dependencies(scene.id()),
            ) {
                batch.scene = None;
                if let Err(reason) = request_material(
                    &mut handles,
                    batch,
                    index,
                    &lazy.native_by_model,
                    &asset_server,
                    &mut standard_materials,
                    time.elapsed_secs(),
                ) {
                    error!(%reason, "cluster material metadata unavailable; stopping before readiness");
                    lazy.failed.push(index);
                    lazy.failed_revision += 1;
                    exit.write(AppExit::error());
                    continue;
                }
                loading += 1;
            } else {
                loading += 1;
                loading_sources.insert(scene.id());
            }
            still_in_flight.push(index);
            continue;
        }
        if let Some(handle) = &batch.loading {
            let id = handle.id();
            let load_failed = asset_failed(&asset_server, id);
            let material_ready = source_asset_ready(
                standard_materials.contains(id),
                batch.key.material_index.is_none() || asset_server.is_loaded_with_dependencies(id),
            );
            if material_ready || load_failed {
                batch.loading = None;
                // The batch's objects start drawing from the cluster path (the stock primitives
                // replacement must be coordinated for each view).
                if load_failed {
                    lazy.failed.push(index);
                    lazy.failed_revision += 1;
                    warn!(
                        model = %batch.key.model_path,
                        material_index = ?batch.key.material_index,
                        state = ?asset_server.load_state(id),
                        "cluster batch material failed to load; stock replacement remains disabled"
                    );
                }
            } else {
                loading += 1;
                still_in_flight.push(index);
                if !batch.reported && time.elapsed_secs() - batch.requested_at > STALLED_SECONDS {
                    batch.reported = true;
                    warn!(
                        model = %batch.key.model_path,
                        material_index = ?batch.key.material_index,
                        state = ?asset_server.load_state(handle.id()),
                        recursive = ?asset_server.recursive_dependency_load_state(handle.id()),
                        "cluster batch material still loading after {STALLED_SECONDS} s"
                    );
                }
            }
        }
    }
    lazy.in_flight = still_in_flight;
    lazy.prefetch_in_flight.retain(|handle| {
        !asset_server.is_loaded_with_dependencies(handle.id())
            && !asset_failed(&asset_server, handle.id())
    });
    let prefetch_only = lazy.prefetch_only_in_flight();
    // When no camera moved, the origin did not shift and no batch changed state, the full look
    // below would see the same requests and terminal failures.
    let counts = (
        lazy.unrequested.len(),
        lazy.in_flight.len(),
        loading,
        prefetch_only,
    );
    let current: Vec<_> = views.iter().map(LazyView::snapshot).collect();
    if !scan.valid
        || scan.layout_scope != lazy.layout_scope
        || scan.failed_revision != lazy.failed_revision
        || scan.offset != offset
        || scan.cameras != current
    {
        scan.failed_visible = lazy
            .failed
            .iter()
            .filter(|&&index| {
                let batch = &lazy.batches[index];
                views.iter().any(|view| {
                    lazy_view_distance(batch.bounds, &batch.spheres, view, offset, LOAD_MARGIN)
                        .is_finite()
                })
            })
            .count();
    }
    if let Some(metrics) = metrics.as_deref_mut()
        && metrics.failed_cluster_batches != scan.failed_visible
    {
        metrics.failed_cluster_batches = scan.failed_visible;
    }
    // With `ClusterBatchPreload` every batch is wanted, nearest first: the startup cover waits for
    // all of them, and nothing the views or a crossing's destination proof depend on changes
    // under them during play. The scan cache must not stop the requests while any are left.
    let unchanged = !(preload && !lazy.unrequested.is_empty())
        && scan.valid
        && scan.layout_scope == lazy.layout_scope
        && scan.failed_revision == lazy.failed_revision
        && scan.offset == offset
        && scan.counts == counts
        && scan.cameras == current;
    if unchanged {
        if let Some(metrics) = metrics.as_deref_mut() {
            metrics.pending_cluster_batches = loading + scan.waiting;
        }
        return;
    }
    scan.valid = true;
    scan.layout_scope = lazy.layout_scope;
    scan.failed_revision = lazy.failed_revision;
    scan.offset = offset;
    scan.counts = counts;
    scan.cameras = current;
    // Unrequested batches with an instance in view, by the distance of the nearest one.
    let mut candidates: Vec<(f32, usize)> = Vec::new();
    for &index in &lazy.unrequested {
        let batch = &lazy.batches[index];
        let mut nearest = f32::INFINITY;
        for view in &views {
            nearest = nearest.min(lazy_view_distance(
                batch.bounds,
                &batch.spheres,
                view,
                offset,
                LOAD_MARGIN,
            ));
        }
        if nearest.is_finite() {
            candidates.push((nearest, index));
        } else if preload {
            // After every batch in view, in index order (the sort is stable).
            candidates.push((f32::MAX, index));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Loading and wanted-but-not-yet-requested batches both hold a view back from settling.
    if let Some(metrics) = metrics.as_deref_mut() {
        metrics.pending_cluster_batches = loading + candidates.len();
    }
    let wanted = candidates.len();
    let mut taken: Vec<usize> = Vec::new();
    let mut source_slots = MAX_LOADING.saturating_sub(loading_sources.len() + prefetch_only);
    let mut restarting_sources: HashMap<_, _> = lazy
        .in_flight
        .iter()
        .filter_map(|&index| {
            let batch = &lazy.batches[index];
            batch
                .source_reload_from
                .zip(batch.scene.as_ref().map(Handle::id))
                .map(|(world, id)| (id, world))
        })
        .collect();
    for (_, index) in candidates {
        // A failure after certification handoff must not bypass a fresh load. Check only
        // this candidate: scanning all retained scenes would add world-sized work each frame.
        let path = &lazy.batches[index].key.model_path;
        if lazy
            .scenes
            .get(path)
            .is_some_and(|handle| asset_failed(&asset_server, handle.id()))
        {
            lazy.scenes.remove(path);
            lazy.scenes_revision += 1;
        }
        // Reusing a speculative source starts no additional I/O and must not be blocked
        // by the capacity that very source already occupies.
        if !lazy
            .scenes
            .contains_key(&lazy.batches[index].key.model_path)
        {
            if source_slots == 0 {
                continue;
            }
            source_slots -= 1;
        }
        let path = lazy.batches[index].key.model_path.clone();
        if !lazy.scenes.contains_key(&path) {
            lazy.scenes_revision += 1;
        }
        let scene = lazy
            .scenes
            .entry(path.clone())
            .or_insert_with(|| asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone())))
            .clone();
        // Request mode retries root failures, but Bevy 0.19 reuses a Loaded scene with a
        // failed image dependency. Restart that cached source once, coalescing its batches.
        if matches!(
            asset_server.load_state(scene.id()),
            bevy::asset::LoadState::Loaded
        ) && asset_failed(&asset_server, scene.id())
            && !lazy.retried_sources.contains(&path)
        {
            // Wait for the old template publication before recording its identity. A mere
            // Loaded flag can precede Assets publication and cannot identify a new attempt.
            let Some(previous) = world_assets.get(scene.id()).map(|asset| asset.world.id()) else {
                // Publication can change without a camera or loader-count change.
                scan.valid = false;
                continue;
            };
            lazy.retried_sources.insert(path.clone());
            restarting_sources.insert(scene.id(), previous);
            asset_server.reload(GltfAssetLabel::Scene(0).from_asset(path.clone()));
            warn!(model = %path, "retrying cached source dependency failure once");
        }
        taken.push(index);
        let batch = &mut lazy.batches[index];
        batch.requested = true;
        batch.requested_at = time.elapsed_secs();
        batch.source_reload_from = restarting_sources.get(&scene.id()).copied();
        // Default-material primitives also need the actual shared source template.
        batch.scene = Some(scene);
    }
    scan.waiting = wanted - taken.len();
    if !taken.is_empty() {
        lazy.unrequested.retain(|index| !taken.contains(index));
        lazy.in_flight.extend(taken);
    }
}

/// AssetServer completion can precede the asset becoming visible in Assets.
/// Both facts are needed before extraction can qualify the source geometry/material.
fn source_asset_ready(present: bool, dependencies_loaded: bool) -> bool {
    present && dependencies_loaded
}

fn asset_failed<A: Asset>(server: &AssetServer, id: AssetId<A>) -> bool {
    matches!(server.load_state(id), bevy::asset::LoadState::Failed(_))
        || matches!(
            server.recursive_dependency_load_state(id),
            bevy::asset::RecursiveDependencyLoadState::Failed(_)
        )
}

/// Requests a batch's base material and spawns the entity that holds it. A batch with no glTF
/// material slot (the default material) is made here directly.
#[allow(clippy::too_many_arguments)]
fn request_material(
    handles: &mut ResMut<ClusterBatchHandles>,
    batch: &mut LazyBatch,
    index: usize,
    native_by_model: &HashMap<String, Vec<bool>>,
    asset_server: &AssetServer,
    standard_materials: &mut Assets<StandardMaterial>,
    now: f32,
) -> Result<(), String> {
    batch.requested_at = now;
    let key = &batch.key;
    let base: Handle<StandardMaterial> = match key.material_index {
        Some(slot) => {
            let native = native_by_model
                .get(&key.model_path)
                .and_then(|slots| slots.get(slot as usize))
                .copied()
                .ok_or_else(|| format!("uncertified material slot {slot} of {}", key.model_path))?;
            asset_server.load(standard_material_path(
                &key.model_path,
                slot,
                key.scale_inverted,
                native,
            ))
        }
        None => standard_materials.add(StandardMaterial::default()),
    };
    batch.loading = Some(base.clone());
    handles.set(index as u32, base);
    Ok(())
}

#[cfg(test)]
#[path = "package/readiness_tests.rs"]
mod readiness_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exclusively_terrain_meshes_skip_generation_including_shared_payloads() {
        let terrain = |mesh| ClusterInstance {
            identity: ClusterInstanceIdentity::Terrain {
                block: mesh,
                worldspace: 42,
                tier: 1,
                anchor: [0, 0],
            },
            mesh,
            batch: 0,
            world_from_local: Mat4::IDENTITY,
            source_worldspace: 42,
        };
        let object = |mesh| ClusterInstance {
            identity: ClusterInstanceIdentity::Object(RecordKey {
                worldspace: 73,
                tier: 1,
                anchor: [2, 3],
                ordinal: mesh,
            }),
            mesh,
            batch: 0,
            world_from_local: Mat4::IDENTITY,
            source_worldspace: 73,
        };
        let instances = [terrain(0), terrain(1), object(1), object(2), terrain(0)];
        assert_eq!(
            terrain_only_meshes(&instances),
            std::collections::HashSet::from([0])
        );
        assert!(terrain_only_meshes(&[object(0)]).is_empty());
        let fixture = ClusterInstance {
            identity: ClusterInstanceIdentity::Fixture,
            ..terrain(0)
        };
        assert!(terrain_only_meshes(&[terrain(0), fixture]).is_empty());
        assert!(terrain_only_meshes(&[]).is_empty());
    }

    #[test]
    fn equal_geometry_keeps_each_terrain_atlas_and_preserves_object_material() {
        use super::super::terrain::{ClusterTerrainMaterial, TerrainBlock, TerrainViewBlock};
        let mut images = Assets::<Image>::default();
        let atlas_a = images.add(Image::default());
        let atlas_b = images.add(Image::default());
        let mut materials = Assets::<ClusterTerrainMaterial>::default();
        let material_a = materials.add(ClusterTerrainMaterial {
            base: StandardMaterial {
                base_color_texture: Some(atlas_a.clone()),
                ..default()
            },
            extension: default(),
        });
        let material_b = materials.add(ClusterTerrainMaterial {
            base: StandardMaterial {
                base_color_texture: Some(atlas_b.clone()),
                ..default()
            },
            extension: default(),
        });
        let row = MeshRow {
            expected: MeshExpectation {
                content_hash: [2; 32],
                payload_bytes: 1,
                mesh_hash: [1; 32],
                primitive_index: 0,
                vertex_count: 3,
                cluster_count: 1,
            },
            payload_path: "clusters/meshes/shared.mcl".into(),
            model_path: "meshes/native-object.glb".into(),
            material_index: Some(4),
        };
        let mut blocks: Vec<_> = [material_a.clone(), material_b.clone()]
            .into_iter()
            .enumerate()
            .map(|(index, material)| TerrainBlock {
                mesh_hash: hex(&row.expected.mesh_hash),
                primitive_index: 0,
                worldspace_id: 70 + index as u32,
                anchor: IVec2::new(index as i32 * 16, 0),
                side_cells: 16,
                instance_path: format!("clusters/terrain/instances/{index}.mci"),
                cells: vec![IVec2::new(index as i32 * 16, 0)],
                material,
                batch: None,
            })
            .collect();
        let object = record_batch_key(&row, ChunkDomain::Object, &blocks, true).unwrap();
        assert_eq!(object.model_path, "meshes/native-object.glb");
        assert_eq!(object.material_index, Some(4));
        assert!(object.scale_inverted);
        let keys: Vec<_> = (0..2)
            .map(|index| {
                record_batch_key(&row, ChunkDomain::Terrain(index), &blocks, false).unwrap()
            })
            .collect();
        assert!(keys[0] != keys[1]);
        assert!(keys.iter().all(|key| key.material_index.is_none()));
        let mut handles = ClusterBatchHandles::default();
        let mut batches = HashMap::new();
        for (index, key) in keys.into_iter().enumerate() {
            let next = batches.len() as u32;
            let batch = *batches.entry(key).or_insert(next);
            bind_terrain_batch(&mut blocks, index as u32, batch, &mut handles).unwrap();
            // Reusing the same resolved batch must still assign that exact block.
            bind_terrain_batch(&mut blocks, index as u32, batch, &mut handles).unwrap();
        }
        assert_eq!(batches.len(), 2);
        assert_eq!(blocks[0].batch, Some(0));
        assert_eq!(blocks[1].batch, Some(1));
        assert_eq!(blocks[0].material, material_a);
        assert_eq!(blocks[1].material, material_b);
        assert_eq!(
            materials.get(&material_a).unwrap().base.base_color_texture,
            Some(atlas_a)
        );
        assert_eq!(
            materials.get(&material_b).unwrap().base.base_color_texture,
            Some(atlas_b)
        );
        assert_eq!(TerrainViewBlock::from(&blocks[0]).batch, Some(0));
        assert_eq!(TerrainViewBlock::from(&blocks[1]).batch, Some(1));
        // Object resolution after terrain binding still uses the untouched GLB slot.
        let after = record_batch_key(&row, ChunkDomain::Object, &blocks, true).unwrap();
        assert!(object == after);
        assert_eq!(row.model_path, "meshes/native-object.glb");
        assert_eq!(row.material_index, Some(4));
        assert!(record_batch_key(&row, ChunkDomain::Terrain(2), &blocks, false).is_err());
        blocks[1].primitive_index = 1;
        assert!(record_batch_key(&row, ChunkDomain::Terrain(1), &blocks, false).is_err());
    }

    #[test]
    fn batch_prefetch_decodes_while_pending_then_reuses_or_drops_exact_handles() {
        use crate::cluster::source_identity::actual_loader_tests::loader_app;
        use bevy::ecs::system::RunSystemOnce;
        for accept in [false, true] {
            let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
            let connection = crate::cluster::overlay_fixture::read_db(directory.path());
            let rows = read_rows(&connection).unwrap();
            let published: Vec<_> = rows
                .values()
                .map(|row| (row.model_path.clone(), String::new()))
                .collect();
            let mut app = loader_app(directory.path());
            app.init_asset::<StandardMaterial>();
            let paths = prefetch_batch_paths(&connection, &published).unwrap();
            assert!(!paths.is_empty());
            let (send, receive) = crossbeam_channel::bounded(1);
            let assets = app.world().resource::<AssetServer>().clone();
            // Dispatch from the same kind of worker as certification, before any App::update.
            // Startup work delaying Update must not delay the first speculative source reads.
            std::thread::scope(|scope| {
                scope
                    .spawn(|| send.send(start_model_prefetch(paths, true, &assets)))
                    .join()
                    .unwrap()
                    .unwrap();
            });
            let initial = receive.try_recv().unwrap();
            assert!(!initial.scenes.is_empty());
            for handle in initial.scenes.values() {
                assert!(assets.get_load_state(handle.id()).is_some());
            }
            send.send(initial).unwrap();
            drop(send);
            app.add_message::<AppExit>()
                .init_resource::<crate::streaming::StreamingMetrics>()
                .insert_resource(PackageValidationStatus::Pending)
                .insert_resource(PendingModelPrefetch::new(receive))
                .insert_resource(PendingPackageValidation(
                    IoTaskPool::get().spawn(future::pending()),
                ))
                .add_systems(
                    Update,
                    (
                        prefetch_pending_models,
                        poll_package_validation,
                        count_blocks_not_resident,
                    )
                        .chain(),
                );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                app.update();
                assert_eq!(
                    app.world().resource::<PackageValidationStatus>(),
                    &PackageValidationStatus::Pending
                );
                assert_eq!(
                    app.world()
                        .resource::<crate::streaming::StreamingMetrics>()
                        .pending_cluster_batches,
                    1
                );
                assert!(!app.world().contains_resource::<ValidatedPackage>());
                assert!(!app.world().contains_resource::<ClusterScene>());
                assert!(!app.world().contains_resource::<LazyBatches>());
                let world = app.world_mut();
                assert_eq!(world.query::<&WorldAssetRoot>().iter(world).count(), 0);
                let prefetched = world.resource::<PendingModelPrefetch>();
                if !prefetched.scenes.is_empty()
                    && prefetched.scenes.values().all(|handle| {
                        world
                            .resource::<AssetServer>()
                            .is_loaded_with_dependencies(handle.id())
                    })
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "speculative source scene failed to decode"
                );
                std::thread::yield_now();
            }
            for handle in app
                .world()
                .resource::<PendingModelPrefetch>()
                .scenes
                .values()
            {
                assert!(
                    app.world()
                        .resource::<Assets<WorldAsset>>()
                        .contains(handle.id())
                );
            }
            if accept {
                // Accept before this additional source has any asset publication. Its I/O
                // reservation must survive both certificate and scene-loader handoffs.
                let pending = app
                    .world()
                    .resource::<Assets<WorldAsset>>()
                    .reserve_handle();
                let mut prefetch = app.world_mut().resource_mut::<PendingModelPrefetch>();
                prefetch
                    .scenes
                    .insert("meshes/pending-handoff.glb".into(), pending.clone());
                prefetch.in_flight.push(pending);
            }
            let outstanding: Vec<_> = app
                .world()
                .resource::<PendingModelPrefetch>()
                .in_flight
                .iter()
                .map(Handle::id)
                .collect();
            let expected: HashMap<_, _> = app
                .world()
                .resource::<PendingModelPrefetch>()
                .scenes
                .iter()
                .map(|(path, handle)| (path.clone(), handle.id()))
                .collect();
            let package = directory.path().to_owned();
            app.insert_resource(PendingPackageValidation(IoTaskPool::get().spawn(
                async move {
                    if accept {
                        crate::cluster::overlay_fixture::open_fixture(&package, false)
                            .map_err(|error| format!("{error:#}"))
                    } else {
                        Err("invented certification failure after prefetch".into())
                    }
                },
            )));
            while app.world().contains_resource::<PendingPackageValidation>() {
                app.update();
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert!(!app.world().contains_resource::<PendingModelPrefetch>());
            if accept {
                assert!(app.world().contains_resource::<ValidatedPackage>());
                assert!(!app.world().contains_resource::<ClusterScene>());
                assert_eq!(
                    app.world()
                        .resource::<PrefetchedBatchModels>()
                        .1
                        .iter()
                        .map(Handle::id)
                        .collect::<Vec<_>>(),
                    outstanding
                );

                assert_eq!(
                    app.world()
                        .resource::<PrefetchedBatchModels>()
                        .0
                        .iter()
                        .map(|(path, handle)| (path.clone(), handle.id()))
                        .collect::<HashMap<_, _>>(),
                    expected
                );
                app.insert_resource(PackageSource {
                    base: directory.path().to_owned(),
                    overlay: crate::cluster::overlay_fixture::overlay_path(directory.path()),
                    terrain: false,
                    priority_point: None,
                })
                .init_resource::<ClusterBatchHandles>();
                app.world_mut()
                    .run_system_once(load_cluster_package)
                    .unwrap();
                assert!(app.world().contains_resource::<ClusterScene>());
                assert_eq!(
                    app.world()
                        .resource::<LazyBatches>()
                        .prefetch_in_flight
                        .iter()
                        .map(Handle::id)
                        .collect::<Vec<_>>(),
                    outstanding
                );
                assert_eq!(
                    app.world()
                        .resource::<LazyBatches>()
                        .scenes
                        .iter()
                        .map(|(path, handle)| (path.clone(), handle.id()))
                        .collect::<HashMap<_, _>>(),
                    expected
                );
            } else {
                assert!(!app.world().contains_resource::<PrefetchedBatchModels>());
                assert!(!app.world().contains_resource::<ValidatedPackage>());
                assert!(!app.world().contains_resource::<ClusterScene>());
                assert!(!app.world().contains_resource::<LazyBatches>());
                // No test-owned strong handles mask a retained prefetch allocation.
                while expected
                    .values()
                    .any(|id| app.world().resource::<Assets<WorldAsset>>().contains(*id))
                {
                    app.update();
                    assert!(
                        std::time::Instant::now() < deadline,
                        "refused prefetch kept source scene assets alive"
                    );
                    std::thread::yield_now();
                }
            }
        }
    }

    #[test]
    fn certification_handoff_drains_prefetch_sent_after_the_frames_receiver_pass() {
        use bevy::ecs::system::RunSystemOnce;
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package =
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
        let mut app =
            crate::cluster::source_identity::actual_loader_tests::loader_app(directory.path());
        app.add_message::<AppExit>();
        let (send, receive) = crossbeam_channel::bounded(1);
        app.insert_resource(PendingModelPrefetch::new(receive));
        app.world_mut()
            .run_system_once(prefetch_pending_models)
            .unwrap();
        let handle = app
            .world()
            .resource::<Assets<WorldAsset>>()
            .reserve_handle();
        let id = handle.id();
        let assets = app.world().resource::<AssetServer>().clone();
        let failed: Handle<WorldAsset> =
            assets.load(GltfAssetLabel::Scene(0).from_asset("meshes/absent-late-prefetch.glb"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !asset_failed(&assets, failed.id()) {
            app.update();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        send.send(ModelPrefetch {
            remaining: default(),
            scenes: HashMap::from([
                ("meshes/handoff.glb".into(), handle.clone()),
                ("meshes/absent-late-prefetch.glb".into(), failed.clone()),
            ]),
            in_flight: vec![handle, failed],
        })
        .unwrap();
        let task = IoTaskPool::get().spawn(async move { Ok(package) });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !task.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        app.insert_resource(PendingPackageValidation(task));
        // No prefetch system or App::update between the late send and certificate acceptance.
        app.world_mut()
            .run_system_once(poll_package_validation)
            .unwrap();
        let prefetched = app.world().resource::<PrefetchedBatchModels>();
        assert_eq!(prefetched.0["meshes/handoff.glb"].id(), id);
        assert_eq!(
            prefetched.0.len(),
            1,
            "late failed speculation was retained"
        );
        assert_eq!(
            prefetched.1.iter().map(Handle::id).collect::<Vec<_>>(),
            [id]
        );
        assert!(!app.world().contains_resource::<PendingModelPrefetch>());
        assert!(app.world().contains_resource::<ValidatedPackage>());
        assert!(!app.world().contains_resource::<ClusterScene>());
    }

    #[test]
    fn speculative_requests_obey_the_loader_limit_and_lazy_launches_bound_total_residency() {
        use bevy::ecs::system::RunSystemOnce;
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("OWNER.txt"),
            "owner: prefetch fixture\n",
        )
        .unwrap();
        for preload_all in [false, true] {
            let mut app =
                crate::cluster::source_identity::actual_loader_tests::loader_app(directory.path());
            let (send, receive) = crossbeam_channel::bounded(1);
            let initial = start_model_prefetch(
                (0..MAX_LOADING * 3)
                    .map(|i| format!("meshes/absent-{i}.glb"))
                    .collect(),
                preload_all,
                app.world().resource::<AssetServer>(),
            );
            assert_eq!(
                initial.in_flight.len(),
                if preload_all { MAX_LOADING } else { 0 }
            );
            assert_eq!(
                initial.scenes.len(),
                if preload_all { MAX_LOADING } else { 0 }
            );
            assert_eq!(
                initial.remaining.len(),
                if preload_all { MAX_LOADING * 2 } else { 0 }
            );
            send.send(initial).unwrap();
            app.insert_resource(PendingModelPrefetch::new(receive));
            app.world_mut()
                .run_system_once(prefetch_pending_models)
                .unwrap();
            let pending = app.world().resource::<PendingModelPrefetch>();
            assert!(pending.in_flight.len() <= MAX_LOADING);
            assert_eq!(
                pending.scenes.len() + pending.remaining.len(),
                if preload_all { MAX_LOADING * 3 } else { 0 }
            );
        }
    }

    #[test]
    fn failed_speculative_scene_is_dropped_and_a_fresh_load_recovers() {
        use bevy::ecs::system::RunSystemOnce;
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let rows = read_rows(&crate::cluster::overlay_fixture::read_db(directory.path())).unwrap();
        let good_path = rows.values().next().unwrap().model_path.clone();
        let path = "meshes/transient-prefetch.glb";
        let mut app =
            crate::cluster::source_identity::actual_loader_tests::loader_app(directory.path());
        app.init_asset::<StandardMaterial>();
        let assets = app.world().resource::<AssetServer>().clone();
        let initial = start_model_prefetch(vec![path.into()], true, &assets);
        let failed_id = initial.scenes[path].id();
        let (send, receive) = crossbeam_channel::bounded(1);
        send.send(initial).unwrap();
        app.insert_resource(PendingModelPrefetch::new(receive));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !asset_failed(&assets, failed_id) {
            app.update();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        app.world_mut()
            .run_system_once(prefetch_pending_models)
            .unwrap();
        assert!(
            !app.world()
                .resource::<PendingModelPrefetch>()
                .scenes
                .contains_key(path),
            "a failed speculative handle must not bypass the lazy loader's fresh load"
        );
        assert!(
            app.world()
                .resource::<PendingModelPrefetch>()
                .in_flight
                .is_empty()
        );
        std::fs::copy(
            directory.path().join(good_path),
            directory.path().join(path),
        )
        .unwrap();
        let retried: Handle<WorldAsset> = assets.load(GltfAssetLabel::Scene(0).from_asset(path));
        while !assets.is_loaded_with_dependencies(retried.id()) {
            app.update();
            assert!(
                std::time::Instant::now() < deadline,
                "fresh request did not recover"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn speculative_model_paths_are_limited_to_published_glbs() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE cluster_meshes(model_path TEXT); INSERT INTO cluster_meshes VALUES ('../outside.glb'), ('meshes/absent.glb'), ('textures/a.ktx2')").unwrap();
        let models =
            prefetch_batch_paths(&connection, &[("textures/a.ktx2".into(), String::new())])
                .unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn failed_requested_package_exits_nonzero_once_and_never_publishes_a_certificate() {
        let mut app = App::new();
        let task = IoTaskPool::get_or_init(Default::default).spawn(async {
            Err::<CertifiedPackage, _>("invented certification failure".to_owned())
        });
        app.add_message::<AppExit>()
            .insert_resource(PackageValidationStatus::Pending)
            .insert_resource(PendingPackageValidation(task))
            .add_systems(Update, poll_package_validation);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.update();
            if matches!(
                app.world().resource::<PackageValidationStatus>(),
                PackageValidationStatus::Failed(_)
            ) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "failure task did not complete"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            app.world().resource::<PackageValidationStatus>(),
            &PackageValidationStatus::Failed("invented certification failure".into())
        );
        assert!(!app.world().contains_resource::<ValidatedPackage>());
        assert!(!app.world().contains_resource::<PendingPackageValidation>());
        let exits = app
            .world()
            .resource::<Messages<AppExit>>()
            .iter_current_update_messages()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(exits, vec![AppExit::error()]);
        app.world_mut().resource_mut::<Messages<AppExit>>().clear();
        for _ in 0..3 {
            app.update();
            assert!(
                app.world()
                    .resource::<Messages<AppExit>>()
                    .iter_current_update_messages()
                    .next()
                    .is_none()
            );
        }
    }

    #[test]
    fn pending_validation_holds_settle_without_a_scene_or_accumulating_counts() {
        let mut app = App::new();
        app.insert_resource(PackageValidationStatus::Pending)
            .init_resource::<crate::streaming::StreamingMetrics>()
            .add_systems(Update, count_blocks_not_resident);
        for _ in 0..3 {
            app.update();
            assert_eq!(
                app.world()
                    .resource::<crate::streaming::StreamingMetrics>()
                    .pending_cluster_batches,
                1
            );
        }
    }

    #[test]
    fn overlay_and_stock_may_spell_the_same_dataset_differently() {
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let base = directory.path();
        let overlay = crate::cluster::overlay_fixture::overlay_path(base);
        checked_package_database(base, &overlay).unwrap();
        checked_package_database(&base.join("."), &overlay).unwrap();
        checked_package_database(
            &base.join("derived").join("..").join("."),
            &base.join("derived/.").join(overlay.file_name().unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn an_overlay_of_another_conversion_of_the_dataset_is_refused() {
        let first = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let second = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let overlay = crate::cluster::overlay_fixture::overlay_path(first.path());
        // Byte-identical conversions are the same conversion: the binding is content identity.
        checked_package_database(second.path(), &overlay).unwrap();
        let manifest = second.path().join("conversion-manifest.json");
        let mut bytes = std::fs::read(&manifest).unwrap();
        bytes.push(b'\n');
        std::fs::write(&manifest, bytes).unwrap();
        let error = checked_package_database(second.path(), &overlay).unwrap_err();
        assert!(
            error.contains("another conversion of this base")
                && error.contains("converter derive-clusters"),
            "{error}"
        );
        assert!(checked_package_database(first.path(), &first.path().join("missing")).is_err());
    }

    #[test]
    fn a_sphere_around_spheres_holds_every_one() {
        let spheres = [
            (Vec3::new(0.0, 0.0, 0.0), 1.0),
            (Vec3::new(10.0, 0.0, 0.0), 2.0),
            (Vec3::new(-4.0, 6.0, 0.0), 0.5),
        ];
        let (center, radius) = enclosing_sphere(&spheres);
        for (sphere_center, sphere_radius) in spheres {
            assert!(center.distance(sphere_center) + sphere_radius <= radius + 1.0e-4);
        }
        assert_eq!(enclosing_sphere(&[]), (Vec3::ZERO, 0.0));
    }

    #[test]
    fn a_hex_identity_is_lowercase_and_64_characters() {
        let text = hex(&[0xab; 32]);
        assert_eq!(text.len(), 64);
        assert!(text.starts_with("abab"));
    }
}

/// Terrain batches for readiness publication, copied without the main-world material assets.
#[derive(Resource, Clone, Default, bevy::render::extract_resource::ExtractResource)]
pub(crate) struct TerrainBatchIds(pub Vec<u32>);

#[cfg(test)]
mod lazy_world_tests {
    use super::*;

    fn view(world: Option<u32>) -> LazyView {
        // The actual default frustum has zero planes and admits every finite positive-radius
        // sphere; geometry-specific margin behavior is tested with an explicit half-space below.
        LazyView {
            frustum: default(),
            transform: GlobalTransform::IDENTITY,
            world,
        }
    }
    fn sphere(world: LazyWorld) -> LazySphere {
        LazySphere {
            center: Vec3::new(0.0, 0.0, -10.0),
            radius: 1.0,
            world,
        }
    }
    fn distance(spheres: &[LazySphere], view: &LazyView) -> f32 {
        lazy_view_distance((Vec3::ZERO, 100.0), spheres, view, Vec3::ZERO, LOAD_MARGIN)
    }

    #[test]
    fn identical_coordinates_only_request_the_placed_world_and_shared_batches_keep_matching_members()
     {
        let other = sphere(LazyWorld::Placed(20));
        assert!(!distance(&[other], &view(Some(10))).is_finite());
        assert!(distance(&[other], &view(Some(20))).is_finite());
        let matching = sphere(LazyWorld::Placed(10));
        assert!(distance(&[other, matching], &view(Some(10))).is_finite());
        assert!(!distance(&[other, matching], &view(None)).is_finite());
        // Hole counters call this exact same helper with zero margin.
        assert!(
            !lazy_view_distance(
                (Vec3::ZERO, 100.0),
                &[other],
                &view(Some(10)),
                Vec3::ZERO,
                0.0
            )
            .is_finite()
        );
    }

    #[test]
    fn matching_world_keeps_load_margin_without_creating_cross_world_demand() {
        let mut camera = view(Some(10));
        camera.frustum.half_spaces[0] = bevy::math::primitives::HalfSpace::new(Vec4::X);
        let item = LazySphere {
            center: Vec3::new(-100.0, 0.0, 0.0),
            radius: 1.0,
            world: LazyWorld::Placed(10),
        };
        let bounds = (item.center, item.radius);
        assert!(!lazy_view_distance(bounds, &[item], &camera, Vec3::ZERO, 0.0).is_finite());
        assert!(lazy_view_distance(bounds, &[item], &camera, Vec3::ZERO, LOAD_MARGIN).is_finite());
        camera.world = Some(20);
        assert!(!lazy_view_distance(bounds, &[item], &camera, Vec3::ZERO, LOAD_MARGIN).is_finite());
    }

    #[test]
    fn public_camera_roles_and_world_changes_keep_main_and_reflection_demand() {
        let main = lazy_camera_world(true, false, Some(10));
        let reflection = lazy_camera_world(false, true, Some(10));
        assert_eq!((main, reflection), (Some(10), Some(10)));
        assert_eq!(lazy_camera_world(false, false, Some(10)), None);
        assert_eq!(lazy_camera_world(true, false, None), None);
        assert_eq!(lazy_camera_world(false, true, None), None);
        let camera = view(main);
        let changed_world = view(Some(20));
        assert_ne!(
            camera.snapshot(),
            changed_world.snapshot(),
            "same-pose world switch must invalidate ScanCache"
        );
        assert_eq!(camera.snapshot(), view(main).snapshot());
    }

    #[test]
    fn fixture_is_explicit_and_missing_certified_object_authority_is_never_a_wildcard() {
        assert_eq!(
            lazy_instance_world(&ClusterInstanceIdentity::Fixture, None),
            LazyWorld::Fixture
        );
        let key = RecordKey {
            worldspace: 10,
            tier: 16,
            anchor: [0, 0],
            ordinal: 0,
        };
        let unknown = lazy_instance_world(&ClusterInstanceIdentity::Object(key), None);
        assert_eq!(unknown, LazyWorld::Unknown);
        assert!(!distance(&[sphere(unknown)], &view(Some(10))).is_finite());
        assert!(distance(&[sphere(LazyWorld::Fixture)], &view(None)).is_finite());
    }

    #[test]
    fn real_certificate_child_follows_placed_world_instead_of_parent_storage_block() {
        let directory = super::super::reader::producer_fixture_tests::published();
        let package =
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
        let (key, record) = package
            .records()
            .find(|(key, record)| key.worldspace != record.worldspace)
            .expect("real producer fixture has child placement in parent block");
        let ownership = lazy_instance_world(&ClusterInstanceIdentity::Object(*key), Some(&package));
        assert_eq!(ownership, LazyWorld::Placed(record.worldspace));
        assert!(!distance(&[sphere(ownership)], &view(Some(key.worldspace))).is_finite());
        assert!(distance(&[sphere(ownership)], &view(Some(record.worldspace))).is_finite());
        assert_eq!(
            lazy_instance_world(&ClusterInstanceIdentity::Fixture, Some(&package)),
            LazyWorld::Unknown
        );
    }
}

#[cfg(test)]
mod empty_certified_loader_tests {
    use super::*;

    #[test]
    fn real_empty_certified_store_publishes_empty_scene_without_gpu_or_placeholder_block() {
        use converter::{
            cache::ConversionManifest,
            cluster::{
                LodMode,
                authority::CoreAuthority,
                probe::ProbeResult,
                publish::{PublishContext, finalize_base},
                stage::{StageSettings, run_stage},
            },
            esm::exporter::create_tables,
        };
        use rusqlite::OpenFlags;
        use shared::asset_lock::AssetLock;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let lock = AssetLock::acquire_exclusive(root).unwrap();
        let db = Connection::open(root.join("skyrim_world.db")).unwrap();
        create_tables(&db).unwrap();
        db.execute("INSERT INTO lod_build VALUES(1,?1)", ["cd".repeat(32)])
            .unwrap();
        db.execute(
            "INSERT INTO worldspaces(id,editor_id,parent_world,flags,lod_origin_x,lod_origin_y) VALUES(112,'Invented',NULL,0,0,0)",
            [],
        )
        .unwrap();
        drop(db);
        let settings = StageSettings {
            mode: LodMode::Cluster,
            triangle_threshold: 1,
            jobs: 1,
            assets_root: None,
            read_database: None,
            area: None,
            child_worldspaces: Vec::new(),
            include_trees: false,
            map_references: true,
        };
        let mut manifest = ConversionManifest::default();
        let probe = ProbeResult {
            passed: true,
            missing: Vec::new(),
            backend: "test".into(),
            adapter_name: "invented empty loader fixture, no GPU".into(),
            json: serde_json::json!({"test":true}),
        };
        let (report, _) = run_stage(
            &settings,
            Some(probe),
            root,
            &root.join("no-previous"),
            &manifest,
        )
        .unwrap();
        let authority = CoreAuthority::open(
            Connection::open_with_flags(
                root.join("skyrim_world.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap(),
            root,
            &manifest,
            &settings,
            report.builder_identity,
        )
        .unwrap();
        let context = PublishContext::new(&lock, root, root).unwrap();
        let base = finalize_base(&context, Some(&authority)).unwrap();
        converter::extensions::refresh_manifest_with_finalized(
            &mut manifest,
            root,
            &[base.extension],
        )
        .unwrap();
        // A real empty store still contains its declared empty folder roots.
        // With no assets the stage does not create these as a side effect.
        for entry in &manifest.extensions {
            for owned in &entry.owns.files {
                if let Some(directory) = owned.strip_suffix('/') {
                    std::fs::create_dir_all(root.join(directory)).unwrap();
                }
            }
        }
        manifest.complete = true;
        manifest
            .save(&root.join("conversion-manifest.json"))
            .unwrap();
        drop(authority);
        drop(lock);
        crate::cluster::overlay_fixture::split_full_copy(root);
        let certificate =
            Arc::new(crate::cluster::overlay_fixture::open_fixture(root, false).unwrap());
        assert_eq!(certificate.records().count(), 0);
        let mut app = App::new();
        app.insert_resource(PackageSource {
            base: root.to_owned(),
            overlay: crate::cluster::overlay_fixture::overlay_path(root),
            terrain: false,
            priority_point: None,
        })
        .insert_resource(ValidatedPackage(certificate.clone()))
        .insert_resource(PackageValidationStatus::Ready)
        .init_resource::<ClusterBatchHandles>()
        .init_resource::<Assets<Image>>()
        .init_resource::<crate::streaming::StreamingMetrics>()
        .add_message::<AppExit>()
        .add_systems(
            Update,
            (
                load_cluster_package,
                count_blocks_not_resident.after(load_cluster_package),
            ),
        );
        app.update();
        let scene = app.world().resource::<ClusterScene>();
        assert!(scene.instances.is_empty() && scene.meshes.is_empty() && scene.blocks.is_empty());
        assert!(Arc::ptr_eq(
            scene.certificate.as_ref().unwrap(),
            &certificate
        ));
        assert!(app.world().resource::<LazyBatches>().batches.is_empty());
        assert_eq!(
            app.world()
                .resource::<crate::streaming::StreamingMetrics>()
                .pending_cluster_batches,
            0
        );
        app.update();
        assert!(app.world().resource::<ClusterScene>().blocks.is_empty());
        // Release actual retained reader guards before tempfile cleanup.
        drop(app);
        drop(certificate);
    }

    #[test]
    fn a_binding_change_after_certification_stops_the_run_instead_of_drawing_none() {
        use bevy::ecs::system::RunSystemOnce;
        let directory = crate::cluster::reader::producer_fixture_tests::published();
        let root = directory.path();
        let certificate =
            Arc::new(crate::cluster::overlay_fixture::open_fixture(root, false).unwrap());
        // Simulate an uncooperative base replacement despite the retained reader lock. The launch
        // receipt retains the checked manifest fingerprint, but each connection must still refuse
        // a changed core identity before it attaches or reads the overlay.
        let database = Connection::open(root.join("skyrim_world.db")).unwrap();
        database
            .execute(
                "UPDATE lod_build SET build_identity = ?1",
                ["ef".repeat(32)],
            )
            .unwrap();
        drop(database);
        let mut app = App::new();
        app.insert_resource(PackageSource {
            base: root.to_owned(),
            overlay: crate::cluster::overlay_fixture::overlay_path(root),
            terrain: false,
            priority_point: None,
        })
        .insert_resource(ValidatedPackage(certificate.clone()))
        .insert_resource(PackageValidationStatus::Ready)
        .init_resource::<ClusterBatchHandles>()
        .init_resource::<Assets<Image>>()
        .add_message::<AppExit>();
        app.world_mut()
            .run_system_once(load_cluster_package)
            .unwrap();
        let exits = app.world().resource::<Messages<AppExit>>();
        assert!(
            exits.iter_current_update_messages().any(AppExit::is_error),
            "requested clusters that no longer open must end the run with an error"
        );
        // Nothing was published before the error: no scene, no batch handles to draw.
        assert!(!app.world().contains_resource::<ClusterScene>());
        assert!(!app.world().contains_resource::<LazyBatches>());
        drop(app);
        drop(certificate);
    }
    fn mutate_payload_for_pinning_test(root: &std::path::Path, corruption: &str) {
        let db = crate::cluster::overlay_fixture::overlay_db(root);
        if corruption == "missing_mesh_row" {
            db.execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM cluster_meshes;")
                .unwrap();
            return;
        }
        let sql = if corruption == "invalid_mesh" {
            "SELECT payload_path FROM cluster_meshes ORDER BY payload_path LIMIT 1"
        } else {
            "SELECT payload_path FROM cluster_chunks ORDER BY payload_path LIMIT 1"
        };
        let relative: String = db.query_row(sql, [], |row| row.get(0)).unwrap();
        drop(db);
        let path = crate::cluster::overlay_fixture::overlay_path(root).join(relative);
        match corruption {
            "missing_block" => std::fs::remove_file(path).unwrap(),
            "invalid_block" | "invalid_mesh" => {
                std::fs::write(path, b"corrupt payload after certification").unwrap();
            }
            "wrong_builder" => {
                use shared::cluster::{InstanceInput, MciInputs, read_mci, write_mci};
                let bytes = std::fs::read(&path).unwrap();
                let block = read_mci(&bytes).unwrap();
                let instances: Vec<_> = (0..block.instance_count as usize)
                    .map(|ordinal| {
                        let record = block.instance(ordinal);
                        InstanceInput {
                            mesh: block.mesh(record.mesh as usize),
                            transform: record.transform,
                            cell_x: record.cell_x,
                            cell_y: record.cell_y,
                            stable_id: record.stable_id,
                            base_form_id: record.base_form_id,
                            snow_material: record.snow_material,
                            source_worldspace_id: record.source_worldspace_id,
                            world_min: block.bounds_min,
                            world_max: block.bounds_max,
                        }
                    })
                    .collect();
                let mut builder = block.builder_identity;
                builder[0] ^= 1;
                let altered = write_mci(&MciInputs {
                    builder_identity: builder,
                    worldspace_id: block.worldspace_id,
                    tier: block.tier,
                    anchor_x: block.anchor_x,
                    anchor_y: block.anchor_y,
                    instances: &instances,
                })
                .unwrap();
                let altered_view = read_mci(&altered).unwrap();
                assert_ne!(altered_view.builder_identity, block.builder_identity);
                assert_eq!(altered_view.instance_count, block.instance_count);
                for ordinal in 0..block.instance_count as usize {
                    assert_eq!(
                        altered_view.instance(ordinal).to_bytes(),
                        block.instance(ordinal).to_bytes(),
                        "wrong-builder fixture must preserve every original instance"
                    );
                }
                std::fs::write(path, altered).unwrap();
            }
            _ => panic!("unknown test corruption: {corruption}"),
        }
    }

    #[test]
    fn corrupt_payloads_before_certification_are_still_refused() {
        for corruption in [
            "missing_block",
            "invalid_block",
            "invalid_mesh",
            "wrong_builder",
            "missing_mesh_row",
        ] {
            let directory = crate::cluster::reader::producer_fixture_tests::published();
            let root = directory.path();
            // Establish a valid producer fixture, then release it before a fresh certification.
            drop(crate::cluster::overlay_fixture::open_fixture(root, false).unwrap());
            mutate_payload_for_pinning_test(root, corruption);
            assert!(
                crate::cluster::overlay_fixture::open_fixture(root, false).is_err(),
                "{corruption}: untrusted inputs must never acquire a certificate"
            );
        }
    }

    #[test]
    fn certified_payloads_stay_pinned_but_missing_mesh_metadata_stops_requested_clusters() {
        use bevy::ecs::system::RunSystemOnce;
        use shared::cluster::{MclSection, read_mci, read_mcl};
        for corruption in [
            "missing_block",
            "invalid_block",
            "invalid_mesh",
            "wrong_builder",
            "missing_mesh_row",
        ] {
            let directory = crate::cluster::reader::producer_fixture_tests::published();
            let root = directory.path();
            let certificate =
                Arc::new(crate::cluster::overlay_fixture::open_fixture(root, false).unwrap());
            let overlay = crate::cluster::overlay_fixture::overlay_path(root);
            let db = crate::cluster::overlay_fixture::overlay_db(root);
            let chunk_paths: Vec<String> = db
                .prepare("SELECT payload_path FROM cluster_chunks ORDER BY payload_path")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            // Independent expected state, captured from the original B before touching the disk.
            // Compare by package-local identity because load_cluster_package reorders instances.
            let mut expected_instances = std::collections::BTreeMap::new();
            let mut expected_mesh_keys = std::collections::BTreeSet::new();
            let mut expected_nonempty_blocks = 0;
            for path in chunk_paths {
                let bytes = std::fs::read(overlay.join(path)).unwrap();
                let block = read_mci(&bytes).unwrap();
                expected_nonempty_blocks += usize::from(block.instance_count != 0);
                for ordinal in 0..block.instance_count {
                    let record = block.instance(ordinal as usize);
                    let mesh = block.mesh(record.mesh as usize);
                    let mesh_key = (hex(&mesh.mesh_hash), mesh.primitive_index);
                    expected_mesh_keys.insert(mesh_key.clone());
                    let t = record.transform;
                    // Independent row-major MCI -> column-major matrix expectation.
                    let columns = [
                        t[0], t[4], t[8], 0.0, t[1], t[5], t[9], 0.0, t[2], t[6], t[10], 0.0, t[3],
                        t[7], t[11], 1.0,
                    ]
                    .map(f32::to_bits);
                    let key = RecordKey {
                        worldspace: block.worldspace_id,
                        tier: block.tier,
                        anchor: [block.anchor_x, block.anchor_y],
                        ordinal,
                    };
                    assert!(
                        expected_instances
                            .insert(key, (columns, record.source_worldspace_id, mesh_key),)
                            .is_none()
                    );
                }
            }
            assert!(
                !expected_instances.is_empty(),
                "positive control must contain geometry"
            );
            let expected_count = expected_instances.len();
            let mesh_paths: Vec<(String, u32, String)> = db
                .prepare("SELECT mesh_hash, primitive_index, payload_path FROM cluster_meshes")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let mut expected_meshes: HashMap<_, _> = mesh_paths
                .into_iter()
                .filter(|(hash, primitive, _)| {
                    expected_mesh_keys.contains(&(hash.clone(), *primitive))
                })
                .map(|(hash, primitive, path)| {
                    (
                        (hash, primitive),
                        std::fs::read(overlay.join(path)).unwrap(),
                    )
                })
                .collect();
            assert_eq!(expected_meshes.len(), expected_mesh_keys.len());
            drop(db);
            mutate_payload_for_pinning_test(root, corruption);
            let mut app = App::new();
            app.insert_resource(PackageSource {
                base: root.to_owned(),
                overlay,
                terrain: false,
                priority_point: None,
            })
            .insert_resource(ValidatedPackage(certificate.clone()))
            .insert_resource(PackageValidationStatus::Ready)
            .init_resource::<ClusterBatchHandles>()
            .init_resource::<Assets<Image>>()
            .add_message::<AppExit>();
            app.world_mut()
                .run_system_once(load_cluster_package)
                .unwrap();
            let exits = app.world().resource::<Messages<AppExit>>();
            if corruption == "missing_mesh_row" {
                // Metadata was not pinned by this repair. Keep the original fail-closed contract.
                assert!(
                    exits.iter_current_update_messages().any(AppExit::is_error),
                    "{corruption}: missing live mesh metadata must stop the requested overlay"
                );
                assert!(!app.world().contains_resource::<ClusterScene>());
                assert!(!app.world().contains_resource::<LazyBatches>());
            } else {
                assert!(
                    exits.iter_current_update_messages().next().is_none(),
                    "{corruption}: an intact retained snapshot must keep the run alive"
                );
                let scene = app.world().resource::<ClusterScene>();
                assert!(Arc::ptr_eq(
                    scene.certificate.as_ref().unwrap(),
                    &certificate
                ));
                assert!(scene.inventory.is_some());
                assert_eq!(scene.instances.len(), expected_count, "{corruption}");
                assert_eq!(scene.blocks.len(), expected_nonempty_blocks, "{corruption}");
                for instance in &scene.instances {
                    let ClusterInstanceIdentity::Object(key) = instance.identity else {
                        panic!("{corruption}: certified object became another identity domain");
                    };
                    let (columns, source_worldspace, mesh_key) = expected_instances
                        .remove(&key)
                        .expect("exactly one drawn instance for each original MCI record");
                    assert_eq!(
                        instance.world_from_local.to_cols_array().map(f32::to_bits),
                        columns,
                        "{corruption}: original transform bits for {key:?}"
                    );
                    assert_eq!(
                        instance.source_worldspace, source_worldspace,
                        "{corruption}"
                    );
                    let mesh = &scene.meshes[instance.mesh as usize];
                    assert_eq!(
                        (hex(&mesh.mesh_hash), mesh.primitive_index),
                        mesh_key,
                        "{corruption}: original mesh binding for {key:?}"
                    );
                    assert!(
                        (instance.batch as usize)
                            < app.world().resource::<LazyBatches>().batches.len()
                    );
                }
                assert!(
                    expected_instances.is_empty(),
                    "{corruption}: no omitted records"
                );
                assert_eq!(scene.meshes.len(), expected_meshes.len(), "{corruption}");
                for mesh in &scene.meshes {
                    let key = (hex(&mesh.mesh_hash), mesh.primitive_index);
                    let original_bytes = expected_meshes
                        .remove(&key)
                        .expect("draw only an original certified mesh, exactly once");
                    let original = read_mcl(&original_bytes).unwrap();
                    // Every exposed header input plus every raw draw section must be exact B.
                    assert_eq!(
                        (
                            mesh.builder_identity,
                            mesh.mesh_hash,
                            mesh.primitive_index,
                            mesh.flags,
                            mesh.vertex_count,
                            mesh.cluster_count,
                            mesh.group_count,
                            mesh.bvh_node_count,
                            mesh.root_error.to_bits(),
                            mesh.bounds.map(f32::to_bits),
                            mesh.uv_min.map(f32::to_bits),
                            mesh.uv_scale.map(f32::to_bits),
                        ),
                        (
                            original.builder_identity,
                            original.mesh_hash,
                            original.primitive_index,
                            original.flags,
                            original.vertex_count,
                            original.cluster_count,
                            original.group_count,
                            original.bvh_node_count,
                            original.root_error.to_bits(),
                            original.bounds.map(f32::to_bits),
                            original.uv_min.map(f32::to_bits),
                            original.uv_scale.map(f32::to_bits),
                        ),
                        "{corruption}: original mesh header"
                    );
                    for section in [
                        MclSection::Positions,
                        MclSection::Normals,
                        MclSection::Uvs,
                        MclSection::Tangents,
                        MclSection::Colours,
                        MclSection::Clusters,
                        MclSection::ClusterVertices,
                        MclSection::ClusterTriangles,
                        MclSection::Groups,
                        MclSection::Bvh,
                    ] {
                        assert_eq!(
                            mesh.section(section).unwrap(),
                            original.section(section),
                            "{corruption}: exact certified {section:?} bytes"
                        );
                    }
                }
                assert!(
                    expected_meshes.is_empty(),
                    "{corruption}: no omitted mesh payload"
                );
                // The published block runs must cover all retained instances, without gaps.
                let mut next = 0;
                for block in &scene.blocks {
                    assert_eq!(block.first_instance, next, "{corruption}");
                    assert!(block.instance_count > 0);
                    next += block.instance_count;
                }
                assert_eq!(next as usize, expected_count, "{corruption}");
            }
            drop(app);
            drop(certificate);
        }
    }
}

#[cfg(test)]
mod shared_material_label_contract {
    use super::*;
    #[test]
    fn shared_labels_match_the_renderers_actual_gltf_labels() {
        for slot in [0, 7, u32::MAX] {
            for inverted in [false, true] {
                for native in [false, true] {
                    let label = GltfAssetLabel::Material {
                        index: slot as usize,
                        is_scale_inverted: inverted,
                    };
                    assert_eq!(
                        standard_material_path("meshes/model.glb", slot, inverted, native),
                        format!(
                            "meshes/model.glb#{label}{}",
                            if native { "/nif" } else { "/std" }
                        )
                    );
                }
            }
        }
    }
}

impl LazyBatches {
    /// Unrequested batches with no instance in view, for ownership demand tests outside this
    /// module. Each batch's only instance sits at `center`.
    #[cfg(test)]
    pub(crate) fn unrequested_for_test(count: usize, center: Vec3) -> Self {
        Self {
            layout_scope: next_layout_scope(),
            batches: (0..count)
                .map(|index| LazyBatch {
                    primitive: 0,
                    key: BatchKey {
                        model_path: format!("demand-fixture-{index}.glb"),
                        material_index: None,
                        scale_inverted: false,
                    },
                    spheres: vec![LazySphere {
                        center,
                        radius: 1.0,
                        world: LazyWorld::Fixture,
                    }],
                    bounds: (center, 1.0),
                    scene: None,
                    source_reload_from: None,
                    loading: None,
                    requested: false,
                    requested_at: 0.0,
                    reported: false,
                })
                .collect(),
            native_by_model: HashMap::new(),
            scenes: HashMap::new(),
            retried_sources: default(),
            prefetch_in_flight: Vec::new(),
            unrequested: (0..count).collect(),
            in_flight: Vec::new(),
            failed: Vec::new(),
            scenes_revision: 0,
            failed_revision: 0,
        }
    }
}

#[cfg(test)]
mod tangent_worker_tests {
    use super::*;

    fn completed_worker(panics: bool) -> std::thread::JoinHandle<Result<(), String>> {
        let worker = std::thread::spawn(move || {
            assert!(!panics, "injected tangent preparation panic");
            Ok(())
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            worker.is_finished(),
            "worker did not reach its terminal state"
        );
        worker
    }

    #[test]
    fn tangent_worker_panic_exits_instead_of_waiting_for_block_readiness() {
        let mut app = App::new();
        app.add_message::<AppExit>()
            .insert_resource(PendingTangents(
                vec![completed_worker(true)],
                Default::default(),
            ))
            .add_systems(Update, poll_tangent_workers);
        app.update();
        assert!(!app.world().contains_resource::<PendingTangents>());
        assert!(matches!(app.world().resource::<PackageValidationStatus>(),
            PackageValidationStatus::Failed(reason) if reason.contains("panicked")));
        assert_eq!(app.world().resource::<Messages<AppExit>>().len(), 1);
    }

    #[test]
    fn joined_successful_tangent_workers_leave_no_pending_resource_or_exit() {
        let mut app = App::new();
        app.add_message::<AppExit>()
            .insert_resource(PendingTangents(
                vec![completed_worker(false)],
                Default::default(),
            ))
            .add_systems(Update, poll_tangent_workers);
        app.update();
        assert!(!app.world().contains_resource::<PendingTangents>());
        assert!(!app.world().contains_resource::<PackageValidationStatus>());
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
    }
}

#[cfg(test)]
mod native_material_metadata_tests {
    use super::*;

    #[test]
    fn native_material_metadata_refuses_missing_short_and_non_glb_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("model.glb");
        assert!(native_materials(&path).is_err());
        for bytes in [b"short".as_slice(), &[0u8; 20]] {
            std::fs::write(&path, bytes).unwrap();
            let error = native_materials(&path).unwrap_err();
            assert!(error.contains("native material metadata"), "{error}");
        }
    }

    #[test]
    fn checked_glb_metadata_retains_ordinary_material_slots() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("model.glb");
        let mut json = br#"{"asset":{"version":"2.0"},"materials":[{}]}"#.to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bytes = b"glTF".to_vec();
        for word in [2, (20 + json.len()) as u32, json.len() as u32, 0x4e4f534a] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&json);
        std::fs::write(path.clone(), &bytes).unwrap();
        assert_eq!(native_materials(&path).unwrap(), vec![false]);
        let mut invalid_json =
            br#"{"asset":{"version":"2.0"},"materials":[{"alphaMode":"INVALID"}]}"#.to_vec();
        while !invalid_json.len().is_multiple_of(4) {
            invalid_json.push(b' ');
        }
        let mut invalid = b"glTF".to_vec();
        for word in [
            2,
            (20 + invalid_json.len()) as u32,
            invalid_json.len() as u32,
            0x4e4f534a,
        ] {
            invalid.extend_from_slice(&word.to_le_bytes());
        }
        invalid.extend_from_slice(&invalid_json);
        std::fs::write(&path, invalid).unwrap();
        assert!(
            native_materials(&path).is_err(),
            "invalid material enum must refuse without a panic"
        );

        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(native_materials(&path).unwrap_err().contains("bounds"));
    }
}

#[cfg(test)]
#[derive(Resource, Default)]
struct LazyScanWork(usize);

#[cfg(test)]
mod resident_count_cache_tests {
    use super::*;

    #[test]
    fn settled_residency_checks_no_blocks_until_the_owner_publishes_a_change() {
        let mesh = ClusterMeshAsset::from_bytes(
            include_bytes!("../../../shared/tests/data/cluster/fixture.mcl").to_vec(),
        )
        .unwrap();
        let mut scene = ClusterScene::new(mesh, &[]);
        scene.blocks.push(SceneBlock {
            first_instance: 0,
            instance_count: 0,
            meshes: Vec::new(),
            ready: Arc::default(),
            resident: Arc::default(),
        });
        let mut cache = BlockResidentCache::default();
        assert_eq!(cache.pending(&scene, false), 1);
        assert_eq!(cache.block_checks, 1);
        for _ in 0..10 {
            assert_eq!(cache.pending(&scene, false), 1);
        }
        assert_eq!(cache.block_checks, 1);
        scene.blocks[0]
            .resident
            .store(true, std::sync::atomic::Ordering::Release);
        scene
            .residency_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        assert_eq!(cache.pending(&scene, false), 0);
        assert_eq!(cache.block_checks, 2);
        for _ in 0..10 {
            assert_eq!(cache.pending(&scene, false), 0);
        }
        assert_eq!(cache.block_checks, 2);
        scene.blocks[0]
            .resident
            .store(false, std::sync::atomic::Ordering::Release);
        scene
            .residency_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        assert_eq!(cache.pending(&scene, false), 1);
        assert_eq!(cache.block_checks, 3);
    }
}

#[cfg(test)]
mod gpu_refusal_tests {
    use super::*;

    #[test]
    fn terminal_gpu_failure_enters_existing_resource_failure_ledger_once() {
        let mut lazy = LazyBatches::unrequested_for_test(3, Vec3::ZERO);
        let before = lazy.failed_key();
        assert!(lazy.refuse_gpu_batch(1));
        assert_eq!(lazy.failed_batches(), [1]);
        assert!(lazy.is_failed(1));
        assert_eq!(lazy.failed_key(), (before.0, before.1 + 1));
        assert!(!lazy.unrequested.contains(&1));
        assert!(!lazy.in_flight.contains(&1));
        let terminal = lazy.failed_key();
        assert!(lazy.refuse_gpu_batch(1));
        assert_eq!(lazy.failed_key(), terminal);
        assert!(!lazy.refuse_gpu_batch(3));
        assert_eq!(lazy.failed_batches(), [1]);
    }
}

#[cfg(test)]
mod upload_failure_composition_tests {
    use super::*;

    #[test]
    fn joined_panic_and_poisoned_follower_keep_one_terminal_outcome() {
        let mesh = ClusterMeshAsset::from_shared_bytes(
            include_bytes!("../../../shared/tests/data/cluster/fixture.mcl")
                .to_vec()
                .into(),
        )
        .unwrap();
        let scene = ClusterScene::new(mesh.clone(), &[]);
        let failures = super::super::render::UploadFailures::default();
        let mut app = App::new();
        app.add_message::<AppExit>().insert_resource(scene.clone());
        let source_tick = app
            .world()
            .get_resource_ref::<ClusterScene>()
            .unwrap()
            .last_changed();
        let first = mesh.clone();
        let panic_worker = std::thread::spawn(move || {
            first.poison_payload_for_test();
            Ok(())
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !panic_worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "panic worker did not finish"
            );
            std::thread::yield_now();
        }
        let receipt = failures.clone();
        let publication = scene.ready_revision.clone();
        let follower = std::thread::spawn(move || {
            let reason = mesh.prepare_tangents().unwrap_err();
            assert!(reason.contains("poisoned"));
            receipt.report(publication, source_tick, reason);
            Ok(())
        });
        while !follower.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "poison follower did not finish"
            );
            std::thread::yield_now();
        }
        app.insert_resource(PendingTangents(
            vec![panic_worker, follower],
            Default::default(),
        ))
        .add_systems(Update, poll_tangent_workers);
        while app.world().contains_resource::<PendingTangents>() {
            assert!(std::time::Instant::now() < deadline, "workers did not join");
            app.update();
            std::thread::yield_now();
        }
        let first_status = app.world().resource::<PackageValidationStatus>().clone();
        let mut main = bevy::render::MainWorld::default();
        std::mem::swap(&mut *main, app.world_mut());
        super::super::render::publish_upload_failures_for_test(&failures, &mut main);
        assert_eq!(
            main.resource::<Messages<AppExit>>().len(),
            1,
            "panic and poisoned follower emitted duplicate exits"
        );
        assert_eq!(
            main.resource::<PackageValidationStatus>(),
            &first_status,
            "the second writer replaced the first terminal reason"
        );
    }
}

#[cfg(test)]
mod tangent_cancellation_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    #[test]
    fn cancelled_tangent_worker_never_touches_an_unstarted_block() {
        let ready = Arc::new(AtomicBool::new(false));
        let prepared = (
            Vec::new(),
            vec![SceneBlock {
                first_instance: 0,
                instance_count: 0,
                meshes: vec![0], // A cancelled task must stop before accessing any mesh.
                ready: ready.clone(),
                resident: Arc::default(),
            }],
            AtomicUsize::new(0),
            Arc::new(AtomicU64::new(0)),
        );
        let cancelled = AtomicBool::new(true);
        prepare_tangent_blocks(&prepared, &Default::default(), &cancelled).unwrap();
        assert!(!ready.load(Ordering::Acquire));
        assert_eq!(prepared.2.load(Ordering::Relaxed), 0);
        assert_eq!(prepared.3.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_failed_tangent_worker_cancels_its_unfinished_peers() {
        let (started_send, started_receive) = std::sync::mpsc::channel();
        let failed = std::thread::spawn(move || {
            started_send.send(()).unwrap();
            Err("failed tangent block".to_owned())
        });
        started_receive
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !failed.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "test worker failed to finish"
            );
            std::thread::yield_now();
        }
        let (release, wait) = std::sync::mpsc::channel();
        let follower = std::thread::spawn(move || {
            wait.recv().unwrap();
            Ok(())
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut app = App::new();
        app.add_message::<AppExit>()
            .insert_resource(PendingTangents(vec![failed, follower], cancelled.clone()))
            .add_systems(Update, poll_tangent_workers);
        app.update();
        let cancelled_before_follower_finished = cancelled.load(Ordering::Acquire);
        release.send(()).unwrap();
        drop(app); // Always release the channel before assertions and joining workers.
        assert!(
            cancelled_before_follower_finished,
            "failed tangent work must cancel peers before they finish"
        );
    }
}
