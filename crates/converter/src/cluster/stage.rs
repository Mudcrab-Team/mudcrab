//! The cluster stage (ADR-0013): probe, classify, build mesh payloads and instance blocks, write
//! the optional tables and return the report. It runs after the terrain LOD compiler, on the staging
//! tree, behind `--lod-mode`.
//!
//! One pass over the unique models loads, classifies and builds (in parallel, per model); a
//! second pass writes one instance block per 16x16-cell block. Mesh payloads are keyed by the
//! source GLB's `output_hash`, so a payload from the previous published output is reused when its
//! builder identity matches: a plugin change rebuilds only instance blocks.
//!
use super::{
    LodMode,
    builder::{MeshStats, build_mesh_payload},
    classify::{
        WorldReference, WorldspaceOrigin, block_of, classify_references, detect_snow_source,
        transform_point,
    },
    glb_input::load_model,
    identity::identity_inputs,
    model::{AlphaMode, ModelPrimitive, Reject, RejectReason},
    probe::{ProbeResult, probe_capabilities},
    publish,
    report::StageReport,
    resolve::{ModelCatalog, published_files, snapshot},
    scope::{Scope, scope_from_settings},
};
use crate::cache::{ConversionManifest, hash_bytes, link_or_copy};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use shared::{
    cluster::{
        CLUSTER_TABLES_DDL, InstanceInput, MciInputs, MeshRef, hex, instance_payload_path,
        mesh_payload_path, parse_hash, read_mci, read_mcl, write_mci_ordered,
    },
    cluster_mapping::{
        CLUSTER_MAPPING_DDL, COVERAGE_VERSION, DIGEST_VERSION, Eligibility, INSTANCE_MAP_VERSION,
    },
    extensions::core_build_identity,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

/// Block side in cells (`LodTier::Tier16`), part of the builder identity.
const BLOCK_TIER: u32 = shared::cluster::BLOCK_SIDE_CELLS;
#[derive(Debug, Clone)]
pub struct StageSettings {
    pub mode: LodMode,
    pub triangle_threshold: u32,
    pub jobs: usize,
    /// Where converted GLBs are read from; `None` means the staging tree itself.
    /// An overlay derivation sets this to its base conversion.
    pub assets_root: Option<PathBuf>,
    /// World database to classify from; `None` means the staging tree's own.
    /// An overlay writes cluster tables to `clusters.db`; this and `assets_root` name the base.
    pub read_database: Option<PathBuf>,
    /// Limits the stage to the references of one worldspace inside a rectangle of exterior cells,
    /// for a small demo output of one area. `None` takes every eligible reference.
    pub area: Option<CellArea>,
    /// Child worldspaces whose references join their parent's blocks (they must share the
    /// parent's coordinates; the caller knows which do). Empty takes none.
    pub child_worldspaces: Vec<u32>,
    /// Also takes references whose base record is a `TREE` (their models are ordinary GLBs). Off
    /// by default: trees have their own LOD path, and a reference is meant to be in one path only.
    pub include_trees: bool,
    /// Writes the reference mapping tables (provenance, instance map, reference coverage) and
    /// leaves out every candidate the authority's recipe does not call eligible. Needs the core
    /// `lod_build.build_identity` in the world database.
    pub map_references: bool,
}

pub use shared::cluster_recipe::scope::CellArea;

/// A built or reused cluster mesh of one primitive.
#[derive(Debug, Clone)]
struct ClusterMesh {
    payload_path: String,
    content_hash: String,
    source_triangles: u64,
    cluster_count: u32,
    group_count: u32,
    vertex_count: u32,
    payload_bytes: u64,
    /// Whole-mesh bounding sphere, mesh space: centre xyz, radius.
    bounds: [f32; 4],
    reused: bool,
}

#[derive(Debug, Clone)]
enum PrimStatus {
    Cluster(ClusterMesh),
    Rejected { reason: String, detail: String },
}

#[derive(Debug, Clone)]
struct PrimOutcome {
    index: u32,
    material_key: String,
    material_index: Option<u32>,
    alpha: AlphaMode,
    alpha_cutoff: Option<f32>,
    double_sided: bool,
    status: PrimStatus,
}

#[derive(Default)]
struct ModelOutcome {
    glb_path: String,
    hash_hex: String,
    prims: Vec<PrimOutcome>,
    loaded: bool,
    known_from_previous: bool,
    load_failed: Option<String>,
    /// A write failed: the stage stops instead of recording a reject that would be replayed.
    fatal: Option<String>,
    fixups: u64,
}

/// What a previous published output knows, if its builder identity matches ours.
struct Previous {
    root: PathBuf,
    meshes: HashMap<String, Vec<PrevPrim>>,
}

struct PrevPrim {
    index: u32,
    kind: PrevKind,
}

enum PrevKind {
    Mesh {
        payload_path: String,
        content_hash: String,
        source_triangles: u64,
        cluster_count: u32,
        vertex_count: u32,
        payload_bytes: u64,
        material_key: String,
        material_index: Option<u32>,
        alpha: AlphaMode,
        alpha_cutoff: Option<f32>,
        double_sided: bool,
    },
    Reject {
        reason: String,
        detail: String,
    },
}

impl Previous {
    fn load(root: &Path, identity_hex: &str) -> Result<Option<Previous>> {
        // An overlay keeps its cluster tables in `clusters.db`; a full-copy package in its world database.
        let overlay = root.join(publish::OVERLAY_STORE);
        let database = match fs::metadata(&overlay) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file(),
                    "previous overlay store is not a regular file"
                );
                overlay
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                root.join(publish::CORE_DATABASE)
            }
            Err(error) => return Err(error.into()),
        };
        match fs::metadata(&database) {
            Ok(metadata) => ensure!(metadata.is_file(), "previous store is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let recorded: String = connection.query_row(
            "SELECT builder_identity FROM cluster_build WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if recorded != identity_hex {
            return Ok(None);
        }
        let mut meshes: HashMap<String, Vec<PrevPrim>> = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT mesh_hash, primitive_index, payload_path, content_hash, source_triangles,
                        cluster_count, vertex_count, payload_bytes, material_key, material_index,
                        alpha_mode, alpha_cutoff, double_sided
                 FROM cluster_meshes",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PrevPrim {
                    index: row.get(1)?,
                    kind: PrevKind::Mesh {
                        payload_path: row.get(2)?,
                        content_hash: row.get(3)?,
                        source_triangles: row.get::<_, i64>(4)? as u64,
                        cluster_count: row.get(5)?,
                        vertex_count: row.get(6)?,
                        payload_bytes: row.get::<_, i64>(7)? as u64,
                        material_key: row.get(8)?,
                        material_index: row.get(9)?,
                        alpha: if row.get::<_, String>(10)? == "mask" {
                            AlphaMode::Mask
                        } else {
                            AlphaMode::Opaque
                        },
                        alpha_cutoff: row.get::<_, Option<f64>>(11)?.map(|value| value as f32),
                        double_sided: row.get::<_, i64>(12)? != 0,
                    },
                },
            ))
        })?;
        for row in rows {
            let (hash, primitive) = row?;
            meshes.entry(hash).or_default().push(primitive);
        }
        let mut statement = connection
            .prepare("SELECT mesh_hash, primitive_index, reason, detail FROM cluster_rejects")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PrevPrim {
                    index: row.get(1)?,
                    kind: PrevKind::Reject {
                        reason: row.get(2)?,
                        detail: row.get(3)?,
                    },
                },
            ))
        })?;
        for row in rows {
            let (hash, primitive) = row?;
            meshes.entry(hash).or_default().push(primitive);
        }
        for prims in meshes.values_mut() {
            prims.sort_by_key(|prim| prim.index);
        }
        Ok(Some(Previous {
            root: root.to_owned(),
            meshes,
        }))
    }
}

/// What the stage keeps of one reference in the declared scope until the tables are written.
struct Seed {
    reference_id: u32,
    placed_worldspace_id: u32,
    source_cell_id: u32,
    digest: String,
    eligibility: Eligibility,
    /// The content hash of the model, for an eligible reference.
    model: Option<String>,
}

/// The reference mapping of one run: the recipe's answer for every reference in scope.
struct MappingPlan {
    built_from: String,
    scope_json: String,
    seeds: Vec<Seed>,
    /// The input digest of each eligible reference, for the instance map.
    digests: HashMap<u32, String>,
    catalog: ModelCatalog,
}

/// Resolves every reference in the declared scope with the shared recipe and removes from the
/// candidates every reference that is not eligible, so no instance is drawn for a reference the
/// coverage calls refused or unknown.
fn prepare_mapping(
    connection: &Connection,
    world: &mut super::classify::ClassifiedWorld,
    scope: &Scope,
    assets: &Path,
    manifest: &ConversionManifest,
    report: &mut StageReport,
) -> Result<MappingPlan> {
    let built_from = core_build_identity(connection)?
        .filter(|identity| !identity.is_empty())
        .ok_or_else(|| {
            eyre!("the reference mapping needs lod_build.build_identity in the world database")
        })?;
    world
        .scoped
        .retain(|reference| scope.contains(reference.block_worldspace_id, reference.cell));
    let catalog = ModelCatalog::new(assets, published_files(manifest))
        .map_err(|problem| eyre!("the conversion manifest is ambiguous: {problem}"))?;
    let mut seeds = Vec::with_capacity(world.scoped.len());
    let mut digests = HashMap::new();
    let mut eligible_ids = HashSet::new();
    let mut removed: BTreeMap<String, u64> = BTreeMap::new();
    for reference in std::mem::take(&mut world.scoped) {
        let resolved = snapshot(&reference, &catalog);
        match &resolved.eligibility {
            Eligibility::Eligible => {
                digests.insert(reference.reference_id, resolved.digest.clone());
                eligible_ids.insert(reference.reference_id);
            }
            Eligibility::NotEligible(reason) | Eligibility::Unknown(reason)
                if reference.candidate.is_some() =>
            {
                *removed.entry(reason.clone()).or_default() += 1;
            }
            _ => {}
        }
        seeds.push(Seed {
            reference_id: reference.reference_id,
            placed_worldspace_id: reference.placed_worldspace_id,
            source_cell_id: reference.source_cell_id,
            digest: resolved.digest,
            eligibility: resolved.eligibility,
            model: resolved.model,
        });
    }
    world
        .eligible
        .retain(|reference| eligible_ids.contains(&reference.reference_id));
    for (reason, count) in removed {
        *report.reference_rejects.entry(reason).or_default() += count;
    }
    Ok(MappingPlan {
        built_from,
        scope_json: scope.json(),
        seeds,
        digests,
        catalog,
    })
}

struct Context<'a> {
    settings: &'a StageSettings,
    cluster_mode: bool,
    staging: &'a Path,
    assets: &'a Path,
    identity: [u8; 32],
    previous: Option<&'a Previous>,
    /// The mapping's model catalog, when it is written.
    catalog: Option<&'a ModelCatalog>,
    space: Option<&'a std::sync::Arc<crate::derive_space::SpaceGuard>>,
}

impl Context<'_> {
    fn write_new(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        match self.space {
            Some(space) => space.with_file(path, bytes.len() as u64, || write_new(path, bytes)),
            None => write_new(path, bytes),
        }
    }
}

/// Runs the stage and returns its report and the artifacts to publish. `previous_output` is the
/// last published package, if any, for payload reuse.
pub fn run_stage(
    settings: &StageSettings,
    early_probe: Option<ProbeResult>,
    staging: &Path,
    previous_output: &Path,
    manifest: &ConversionManifest,
) -> Result<(StageReport, Vec<PathBuf>)> {
    run_stage_guarded(
        settings,
        early_probe,
        staging,
        previous_output,
        manifest,
        None,
    )
}

pub(crate) fn run_stage_guarded(
    settings: &StageSettings,
    early_probe: Option<ProbeResult>,
    staging: &Path,
    previous_output: &Path,
    manifest: &ConversionManifest,
    space: Option<&std::sync::Arc<crate::derive_space::SpaceGuard>>,
) -> Result<(StageReport, Vec<PathBuf>)> {
    let inputs = identity_inputs(settings.triangle_threshold);
    let identity = inputs.identity();
    let identity_hex = inputs.identity_hex();
    let mut report = StageReport {
        requested_mode: settings.mode.to_string(),
        mode: LodMode::Baked.to_string(),
        builder_identity: identity_hex.clone(),
        triangle_threshold: settings.triangle_threshold,
        jobs: settings.jobs,
        ..Default::default()
    };

    // Probe: the converter asks before any LOD work (the pipeline passes the early result); a
    // forced mode overrides the answer but still records what the adapter reported.
    let probed = match settings.mode {
        LodMode::Baked => None,
        _ => Some(early_probe.unwrap_or_else(probe_capabilities)),
    };
    let probe_json = match (&settings.mode, probed) {
        (LodMode::Baked, _) | (_, None) => None,
        (mode, Some(probe)) => {
            let mut json = probe.json.clone();
            if *mode == LodMode::Cluster {
                json["forced"] = json!(true);
            }
            report.probe = Some(json.clone());
            if !probe.passed && *mode == LodMode::Auto {
                report.notices.push(format!(
                    "auto: the GPU probe failed ({}); using baked mode",
                    probe.missing.join(", ")
                ));
            }
            (probe.passed || *mode == LodMode::Cluster).then_some(json)
        }
    };
    let cluster_mode = probe_json.is_some();
    if cluster_mode {
        report.mode = LodMode::Cluster.to_string();
    }
    if !cluster_mode {
        return Ok((report, Vec::new()));
    }

    // An overlay staging folder holds its own `clusters.db` and nothing of the base: core tables
    // and files are read from the base in place, cluster tables are written to `clusters.db`.
    let overlay = staging.join(publish::OVERLAY_STORE).is_file();
    ensure!(
        !overlay || (settings.assets_root.is_some() && settings.read_database.is_some()),
        "a cluster overlay stage reads the base conversion: it needs assets_root and read_database"
    );
    let database_path = settings
        .read_database
        .clone()
        .unwrap_or_else(|| staging.join(publish::CORE_DATABASE));
    if !database_path.is_file() {
        report
            .notices
            .push("no world database: the cluster stage has nothing to do".into());
        return Ok((report, Vec::new()));
    }
    let connection = if settings.read_database.is_some() {
        Connection::open_with_flags(&database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
    } else {
        Connection::open(&database_path)?
    };

    let mut world = classify_references(
        &connection,
        &settings.child_worldspaces,
        settings.include_trees,
    )?;
    if !world.skipped_children.is_empty() {
        report.notices.push(format!(
            "child worldspaces without a parent that has a LOD origin were skipped: {:?}",
            world.skipped_children
        ));
    }
    let mut per_source: BTreeMap<u32, u64> = BTreeMap::new();
    for reference in &world.eligible {
        if reference.source_worldspace_id != 0 {
            *per_source
                .entry(reference.source_worldspace_id)
                .or_default() += 1;
        }
    }
    for (source, count) in &per_source {
        report.notices.push(format!(
            "child worldspace {source}: {count} eligible references join the blocks of their parent"
        ));
    }
    if let Some(area) = settings.area {
        let before = world.eligible.len();
        world.eligible.retain(|reference| {
            area.contains(reference.worldspace_id, reference.cell_x, reference.cell_y)
        });
        report.notices.push(format!(
            "area limit: worldspace {} cells ({}, {}) to ({}, {}): {} of {before} eligible references kept",
            area.worldspace_id,
            area.min.0,
            area.min.1,
            area.max.0,
            area.max.1,
            world.eligible.len()
        ));
    }
    report.reference_rejects = world.rejected.clone();
    let scope = scope_from_settings(settings, detect_snow_source(&connection)?);
    let mapping_plan = if cluster_mode && settings.map_references {
        Some(prepare_mapping(
            &connection,
            &mut world,
            &scope,
            settings.assets_root.as_deref().unwrap_or(staging),
            manifest,
            &mut report,
        )?)
    } else {
        None
    };
    report.counts.references_examined = world.examined;
    report.counts.references_eligible = world.eligible.len() as u64;

    // Unique models, by converted GLB path, with the hash the manifest recorded for each.
    let hashes: HashMap<&str, &str> = manifest
        .entries
        .values()
        .filter(|entry| entry.output.starts_with("meshes/") && entry.output.ends_with(".glb"))
        .map(|entry| (entry.output.as_str(), entry.output_hash.as_str()))
        .collect();
    let mut references_of: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut missing_paths: BTreeSet<&str> = BTreeSet::new();
    for (index, reference) in world.eligible.iter().enumerate() {
        match hashes.get(reference.glb_path.as_str()) {
            Some(_) => references_of
                .entry(reference.glb_path.as_str())
                .or_default()
                .push(index),
            None => {
                missing_paths.insert(reference.glb_path.as_str());
                *report
                    .reference_rejects
                    .entry("model_not_converted".into())
                    .or_default() += 1;
            }
        }
    }
    // Two paths with the same converted bytes share one payload set: process each hash once, under
    // the first path in sorted order, and map every path to that outcome.
    let mut model_list: Vec<(&str, &str)> = Vec::new();
    let mut by_hash: HashMap<&str, usize> = HashMap::new();
    let mut path_to_model: HashMap<&str, usize> = HashMap::new();
    for path in references_of.keys() {
        let hash = hashes[path];
        let index = *by_hash.entry(hash).or_insert_with(|| {
            // With the mapping on, the payload is built under the canonical path of the content
            // (the first published path), which is the path the authority derives materials from.
            let built_under = mapping_plan
                .as_ref()
                .and_then(|plan| plan.catalog.canonical_path(hash))
                .unwrap_or(*path);
            model_list.push((built_under, hash));
            model_list.len() - 1
        });
        path_to_model.insert(*path, index);
    }
    report.counts.models = model_list.len() as u64;
    report.counts.models_missing_from_manifest = missing_paths.len() as u64;

    let previous = if cluster_mode {
        match Previous::load(previous_output, &identity_hex) {
            Ok(previous) => previous,
            Err(error) => {
                report.notices.push(format!(
                    "previous cluster store at {} could not be read: {error:#}; rebuilding from the base; inspect or replace the old overlay if this repeats",
                    previous_output.display()
                ));
                None
            }
        }
    } else {
        None
    };
    let context = Context {
        settings,
        cluster_mode,
        staging,
        assets: settings.assets_root.as_deref().unwrap_or(staging),
        identity,
        previous: previous.as_ref(),
        catalog: mapping_plan.as_ref().map(|plan| &plan.catalog),
        space,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(settings.jobs)
        .build()
        .wrap_err("failed to create the cluster stage worker pool")?;

    // Pass 1: load, classify, build.
    let outcomes: Vec<ModelOutcome> = pool.install(|| {
        model_list
            .par_iter()
            .map(|(path, hash)| process_model(&context, path, hash))
            .collect()
    });
    if let Some(fatal) = outcomes.iter().find_map(|outcome| outcome.fatal.as_ref()) {
        return Err(eyre!("cluster stage: {fatal}"));
    }

    for outcome in &outcomes {
        report.counts.models_loaded += u64::from(outcome.loaded);
        report.counts.models_known_from_previous += u64::from(outcome.known_from_previous);
        report.counts.models_failed_to_load += u64::from(outcome.load_failed.is_some());
        report.counts.monotonic_fixups += outcome.fixups;
        if let Some(error) = &outcome.load_failed {
            report
                .notices
                .push(format!("{}: {error}", outcome.glb_path));
        }
        for prim in &outcome.prims {
            report.counts.primitives += 1;
            match &prim.status {
                PrimStatus::Cluster(mesh) => {
                    report.counts.primitives_clustered += 1;
                    report.counts.clusters += u64::from(mesh.cluster_count);
                    report.counts.groups += u64::from(mesh.group_count);
                    report.counts.source_triangles_clustered += mesh.source_triangles;
                    report.bytes.mesh_payloads += mesh.payload_bytes;
                    if mesh.reused {
                        report.counts.meshes_reused += 1;
                    } else {
                        report.counts.meshes_built += 1;
                    }
                }
                PrimStatus::Rejected { reason, .. } => {
                    *report.primitive_rejects.entry(reason.clone()).or_default() += 1;
                }
            }
        }
    }

    // Pass 2: blocks.
    let mut blocks: BTreeMap<(u32, i32, i32), BlockWork> = BTreeMap::new();
    let origins: HashMap<u32, WorldspaceOrigin> = world
        .worldspaces
        .iter()
        .map(|origin| (origin.worldspace_id, *origin))
        .collect();
    for reference in &world.eligible {
        let Some(&model) = path_to_model.get(reference.glb_path.as_str()) else {
            continue;
        };
        let outcome = &outcomes[model];
        let origin = origins[&reference.worldspace_id].origin;
        let anchor = block_of(origin, reference.cell_x, reference.cell_y);
        let work = blocks
            .entry((reference.worldspace_id, anchor.0, anchor.1))
            .or_default();
        for prim in &outcome.prims {
            if let PrimStatus::Cluster(mesh) = &prim.status {
                work.cluster
                    .push(instance_input(outcome, prim.index, mesh, reference));
                if let Some(plan) = &mapping_plan {
                    work.references.push((
                        reference.reference_id,
                        plan.digests[&reference.reference_id].clone(),
                    ));
                }
            }
        }
    }
    report.counts.blocks = blocks
        .values()
        .filter(|work| !work.cluster.is_empty())
        .count() as u64;

    let block_list: Vec<_> = blocks.into_iter().collect();
    let results: Vec<Result<BlockResult>> = pool.install(|| {
        block_list
            .par_iter()
            .map(|(key, work)| process_block(&context, *key, work))
            .collect()
    });

    let mut chunk_rows = Vec::new();
    let mut artifacts: Vec<PathBuf> = Vec::new();
    for result in results {
        let result = result?;
        if let Some(row) = result.chunk {
            report.counts.instances += u64::from(row.instance_count);
            report.bytes.instance_payloads += row.payload_bytes;
            artifacts.push(PathBuf::from(&row.payload_path));
            chunk_rows.push(row);
        }
    }

    // Tables and payload inventory.
    if cluster_mode {
        let store = crate::derive_sqlite::open(&publish::store_database(staging), space)?;
        write_tables(
            &store,
            &identity_hex,
            settings.triangle_threshold,
            &outcomes,
            &chunk_rows,
            mapping_plan.as_ref(),
        )?;
        drop(store);
        for outcome in &outcomes {
            for prim in &outcome.prims {
                if let PrimStatus::Cluster(mesh) = &prim.status {
                    artifacts.push(PathBuf::from(&mesh.payload_path));
                }
            }
        }
        // `cluster-manifest.json` is not written here: it is derived from the final store by
        // `publish::finalize_base`, under the dataset's exclusive lock, once every stage that adds
        // geometry has finished.
    }

    artifacts.sort();
    artifacts.dedup();
    if let Some(space) = space {
        space.check_failure()?;
    }
    Ok((report, artifacts))
}

fn instance_input(
    outcome: &ModelOutcome,
    primitive_index: u32,
    mesh: &ClusterMesh,
    reference: &WorldReference,
) -> InstanceInput {
    let hash = parse_hash(&outcome.hash_hex).expect("manifest hashes are SHA-256 hex");
    let centre = transform_point(
        &reference.transform,
        [mesh.bounds[0], mesh.bounds[1], mesh.bounds[2]],
    );
    // Largest column norm of the linear part bounds the scale of the sphere radius.
    let t = &reference.transform;
    let scale = [[t[0], t[4], t[8]], [t[1], t[5], t[9]], [t[2], t[6], t[10]]]
        .iter()
        .map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt())
        .fold(0.0f32, f32::max);
    let radius = mesh.bounds[3] * scale;
    // Bounds are kept in Creation axes (x east, y north, z up), like `lod_chunks`, so the same
    // R-tree query serves both; the transform stays in runtime axes.
    let (min_rt, max_rt) = (
        [centre[0] - radius, centre[1] - radius, centre[2] - radius],
        [centre[0] + radius, centre[1] + radius, centre[2] + radius],
    );
    InstanceInput {
        mesh: MeshRef {
            mesh_hash: hash,
            primitive_index,
        },
        transform: reference.transform,
        cell_x: reference.cell_x,
        cell_y: reference.cell_y,
        // Identity is the final writer ordinal, bound to reference_id in cluster_instance_map.
        // This reserved format field is not used to identify runtime instances.
        stable_id: [0; 16],
        base_form_id: reference.base_form_id,
        snow_material: reference.snow_material,
        source_worldspace_id: reference.source_worldspace_id,
        world_min: [min_rt[0], -max_rt[2], min_rt[1]],
        world_max: [max_rt[0], -min_rt[2], max_rt[1]],
    }
}

fn process_model(context: &Context<'_>, glb_path: &str, hash_hex: &str) -> ModelOutcome {
    let mut outcome = ModelOutcome {
        glb_path: glb_path.to_owned(),
        hash_hex: hash_hex.to_owned(),
        ..Default::default()
    };
    let Some(hash) = parse_hash(hash_hex) else {
        outcome.load_failed = Some(format!("manifest hash {hash_hex} is not SHA-256 hex"));
        return outcome;
    };

    // A model the previous output fully knows costs nothing: link its payloads, read nothing.
    if context.cluster_mode
        && let Some(previous) = context.previous
        && let Some(known) = previous.meshes.get(hash_hex)
        && let Some(prims) = reuse_known(context, previous, hash_hex, known)
        // A payload built under another path's material keys is not this content's now.
        && context.catalog.is_none_or(|catalog| {
            catalog.materials_by_hash(hash_hex).is_some_and(|expected| {
                prims.iter().all(|prim| {
                    !matches!(prim.status, PrimStatus::Cluster(_))
                        || expected.get(&prim.index).is_some_and(|source| {
                            source.material_key == prim.material_key
                                && source.material_index == prim.material_index
                                && source.alpha_mask == (prim.alpha == AlphaMode::Mask)
                                && source.alpha_cutoff.map(f32::to_bits)
                                    == prim.alpha_cutoff.map(f32::to_bits)
                                && source.double_sided == prim.double_sided
                        })
                })
            })
        })
    {
        outcome.prims = prims;
        outcome.known_from_previous = true;
        return outcome;
    }

    let loaded = fs::read(context.assets.join(glb_path))
        .wrap_err("cannot read the converted GLB")
        .and_then(|bytes| {
            // With the reference mapping on, the geometry that is built is the geometry the
            // manifest (and so the authority) vouches for, not whatever is on disk now.
            ensure!(
                !context.settings.map_references || hash_bytes(&bytes) == hash_hex,
                "the converted GLB no longer hashes to the manifest's content hash"
            );
            load_model(&bytes, glb_path)
        });
    let primitives = match loaded {
        Ok(primitives) => primitives,
        Err(error) => {
            outcome.load_failed = Some(format!("{error:#}"));
            return outcome;
        }
    };
    outcome.loaded = true;

    let threshold = context.settings.triangle_threshold as usize;
    for primitive in primitives {
        let triangles = primitive.geometry.triangle_count();
        let status = {
            match (&primitive.reject, triangles < threshold) {
                (Some(reject), _) => reject_status(reject),
                (None, true) => reject_status(&Reject::new(
                    RejectReason::BelowTriangleThreshold,
                    format!("{triangles} triangles under {threshold}"),
                )),
                (None, false) => {
                    match build_or_reuse(context, &mut outcome, &primitive, hash, hash_hex) {
                        Ok(mesh) => PrimStatus::Cluster(mesh),
                        Err(reject) => reject_status(&reject),
                    }
                }
            }
        };
        outcome.prims.push(PrimOutcome {
            index: primitive.index,
            material_key: primitive.material_key.clone(),
            material_index: primitive.material_index,
            alpha: primitive.alpha,
            alpha_cutoff: primitive.alpha_cutoff,
            double_sided: primitive.double_sided,
            status,
        });
    }
    outcome
}

fn reject_status(reject: &Reject) -> PrimStatus {
    PrimStatus::Rejected {
        reason: reject.reason.as_str().to_owned(),
        detail: reject.detail.clone(),
    }
}

/// Builds the payload of `primitive` or links the previous output's identical one.
fn build_or_reuse(
    context: &Context<'_>,
    outcome: &mut ModelOutcome,
    primitive: &ModelPrimitive,
    hash: [u8; 32],
    hash_hex: &str,
) -> Result<ClusterMesh, Reject> {
    let payload_path = mesh_payload_path(hash_hex, primitive.index);
    if let Some(previous) = context.previous
        && let Some(prims) = previous.meshes.get(hash_hex)
        && let Some(prim) = prims.iter().find(|prim| prim.index == primitive.index)
        && let PrevKind::Mesh {
            payload_path: previous_path,
            content_hash,
            source_triangles,
            cluster_count,
            vertex_count,
            payload_bytes,
            ..
        } = &prim.kind
        && let Some(reused) = link_previous(
            context,
            previous,
            previous_path,
            &payload_path,
            content_hash,
            *payload_bytes,
            (hash, primitive.index),
        )
    {
        let (group_count, bounds) = header_facts(&reused);
        return Ok(ClusterMesh {
            payload_path,
            content_hash: content_hash.clone(),
            source_triangles: *source_triangles,
            cluster_count: *cluster_count,
            group_count,
            vertex_count: *vertex_count,
            payload_bytes: *payload_bytes,
            bounds,
            reused: true,
        });
    }

    let built = build_mesh_payload(primitive, context.identity, hash)?;
    let content_hash = hash_bytes(&built.bytes);
    let destination = context.staging.join(&payload_path);
    if let Err(error) = context.write_new(&destination, &built.bytes) {
        outcome.fatal = Some(format!("{payload_path}: {error:#}"));
        return Err(Reject::new(RejectReason::BuilderFailed, "write failed"));
    }
    let MeshStats {
        source_triangles,
        vertex_count,
        cluster_count,
        group_count,
        payload_bytes,
        monotonic_fixups,
        bounds,
        ..
    } = built.stats;
    outcome.fixups += u64::from(monotonic_fixups);
    Ok(ClusterMesh {
        payload_path,
        content_hash,
        source_triangles,
        cluster_count,
        group_count,
        vertex_count,
        payload_bytes,
        bounds,
        reused: false,
    })
}

/// Links a previous payload into staging when its size and builder identity still match.
fn link_previous(
    context: &Context<'_>,
    previous: &Previous,
    previous_path: &str,
    payload_path: &str,
    content_hash: &str,
    payload_bytes: u64,
    expected: ([u8; 32], u32),
) -> Option<Vec<u8>> {
    if !std::path::Path::new(previous_path)
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let source = previous.root.join(previous_path);
    let metadata = fs::metadata(&source).ok()?;
    if metadata.len() != payload_bytes {
        return None;
    }
    let bytes = fs::read(&source).ok()?;
    // The reader's own checks, so a damaged or foreign file is rebuilt, not trusted.
    let view = read_mcl(&bytes).ok()?;
    if view.builder_identity != context.identity
        || (view.mesh_hash, view.primitive_index) != expected
        || hash_bytes(&bytes) != content_hash
    {
        return None;
    }
    let destination = context.staging.join(payload_path);
    let reuse = || {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::remove_file(&destination);
        link_or_copy(&source, &destination)?;
        Ok(())
    };
    match context.space {
        // A refusal is sticky even though payload reuse treats ordinary IO errors as misses.
        Some(space) => space.with_file(&destination, payload_bytes, reuse).ok()?,
        None => reuse().ok()?,
    }
    Some(bytes)
}

/// Group count and bounding sphere from a payload's header.
fn header_facts(bytes: &[u8]) -> (u32, [f32; 4]) {
    let group_count = u32::from_le_bytes(bytes[88..92].try_into().expect("header"));
    let mut bounds = [0f32; 4];
    for (index, value) in bounds.iter_mut().enumerate() {
        *value = f32::from_le_bytes(
            bytes[100 + index * 4..104 + index * 4]
                .try_into()
                .expect("header"),
        );
    }
    (group_count, bounds)
}

/// The statuses of a model whose primitives all come from the previous output, with their payloads
/// linked into staging. `None` when any payload cannot be reused (the model is then rebuilt).
fn reuse_known(
    context: &Context<'_>,
    previous: &Previous,
    hash_hex: &str,
    known: &[PrevPrim],
) -> Option<Vec<PrimOutcome>> {
    let hash = parse_hash(hash_hex)?;
    let mut prims = Vec::with_capacity(known.len());
    for prim in known {
        let status = match &prim.kind {
            // A builder failure may have been a one-off (allocation, I/O): rebuild, do not replay.
            PrevKind::Reject { reason, .. } if reason == RejectReason::BuilderFailed.as_str() => {
                return None;
            }
            PrevKind::Reject { reason, detail } => PrimStatus::Rejected {
                reason: reason.clone(),
                detail: detail.clone(),
            },
            PrevKind::Mesh {
                payload_path,
                content_hash,
                source_triangles,
                cluster_count,
                vertex_count,
                payload_bytes,
                material_key,
                material_index,
                alpha,
                alpha_cutoff,
                double_sided,
            } => {
                let destination = mesh_payload_path(hash_hex, prim.index);
                let bytes = link_previous(
                    context,
                    previous,
                    payload_path,
                    &destination,
                    content_hash,
                    *payload_bytes,
                    (hash, prim.index),
                )?;
                let (group_count, bounds) = header_facts(&bytes);
                prims.push(PrimOutcome {
                    index: prim.index,
                    material_key: material_key.clone(),
                    material_index: *material_index,
                    alpha: *alpha,
                    alpha_cutoff: *alpha_cutoff,
                    double_sided: *double_sided,
                    status: PrimStatus::Cluster(ClusterMesh {
                        payload_path: destination,
                        content_hash: content_hash.clone(),
                        source_triangles: *source_triangles,
                        cluster_count: *cluster_count,
                        group_count,
                        vertex_count: *vertex_count,
                        payload_bytes: *payload_bytes,
                        bounds,
                        reused: true,
                    }),
                });
                continue;
            }
        };
        prims.push(PrimOutcome {
            index: prim.index,
            material_key: String::new(),
            material_index: None,
            alpha: AlphaMode::Opaque,
            alpha_cutoff: None,
            double_sided: false,
            status,
        });
    }
    Some(prims)
}

/// Creates `path` with `bytes`, replacing a hard-linked or stale file instead of writing through it.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::durability::replace_file(path, bytes)
        .wrap_err_with(|| format!("failed to write {}", path.display()))
}

// Blocks ------------------------------------------------------------------------------------

#[derive(Default)]
struct BlockWork {
    cluster: Vec<InstanceInput>,
    /// The reference and input digest of each entry of `cluster`, when the mapping is written.
    references: Vec<(u32, String)>,
}

struct ChunkRow {
    worldspace_id: u32,
    anchor: (i32, i32),
    payload_path: String,
    content_hash: String,
    input_key: String,
    bounds: ([f32; 3], [f32; 3]),
    instance_count: u32,
    payload_bytes: u64,
    source_cells: String,
    /// One row per record in the writer's final order, when the mapping is written.
    map: Vec<MapRow>,
}

/// One row of `cluster_instance_map`, minus the block's key.
struct MapRow {
    ordinal: u32,
    reference_id: u32,
    placed_worldspace_id: u32,
    cell: (i32, i32),
    model_hash: String,
    primitive_index: u32,
    digest: String,
}

struct BlockResult {
    chunk: Option<ChunkRow>,
}

fn process_block(
    context: &Context<'_>,
    key: (u32, i32, i32),
    work: &BlockWork,
) -> Result<BlockResult> {
    let (worldspace_id, anchor_x, anchor_y) = key;
    let mut result = BlockResult { chunk: None };

    if !work.cluster.is_empty() {
        let (bytes, order) = write_mci_ordered(&MciInputs {
            builder_identity: context.identity,
            worldspace_id,
            tier: BLOCK_TIER,
            anchor_x,
            anchor_y,
            instances: &work.cluster,
        })
        .map_err(|error| eyre!("instance block {key:?}: {error}"))?;
        let view = read_mci(&bytes).map_err(|error| eyre!("instance block {key:?}: {error}"))?;
        let payload_path =
            instance_payload_path(worldspace_id, BLOCK_TIER as i32, anchor_x, anchor_y);
        context.write_new(&context.staging.join(&payload_path), &bytes)?;
        let written = fs::read(context.staging.join(&payload_path))?;
        read_mci(&written)
            .map_err(|error| eyre!("instance block {payload_path} on disk: {error}"))?;
        let content_hash = hash_bytes(&written);
        result.chunk = Some(ChunkRow {
            worldspace_id,
            anchor: (anchor_x, anchor_y),
            payload_path,
            // Rebuilding instance blocks binds their ordinal map to the exact bytes written.
            input_key: content_hash.clone(),
            content_hash,
            bounds: (view.bounds_min, view.bounds_max),
            instance_count: view.instance_count,
            payload_bytes: bytes.len() as u64,
            source_cells: work
                .cluster
                .iter()
                .map(|instance| (instance.cell_x, instance.cell_y))
                .collect::<BTreeSet<_>>()
                .iter()
                .map(|(x, y)| format!("{x},{y}"))
                .collect::<Vec<_>>()
                .join(";"),
            // The final ordinal of each record is its position in the writer's order, so the map
            // is bound to the bytes that were written, not to the order they were queued in.
            map: if work.references.is_empty() {
                Vec::new()
            } else {
                order
                    .iter()
                    .enumerate()
                    .map(|(ordinal, &input)| {
                        let instance = &work.cluster[input];
                        MapRow {
                            ordinal: ordinal as u32,
                            reference_id: work.references[input].0,
                            placed_worldspace_id: if instance.source_worldspace_id != 0 {
                                instance.source_worldspace_id
                            } else {
                                worldspace_id
                            },
                            cell: (instance.cell_x, instance.cell_y),
                            model_hash: hex(&instance.mesh.mesh_hash),
                            primitive_index: instance.mesh.primitive_index,
                            digest: work.references[input].1.clone(),
                        }
                    })
                    .collect()
            },
        });
    }

    Ok(result)
}

// Tables ------------------------------------------------------------------------------------

fn write_tables(
    connection: &Connection,
    identity_hex: &str,
    triangle_threshold: u32,
    outcomes: &[ModelOutcome],
    chunks: &[ChunkRow],
    mapping: Option<&MappingPlan>,
) -> Result<()> {
    connection.execute_batch(CLUSTER_TABLES_DDL)?;
    if mapping.is_some() {
        connection.execute_batch(CLUSTER_MAPPING_DDL)?;
    }
    let transaction = connection.unchecked_transaction()?;
    let mut tables = vec![
        "cluster_build",
        "cluster_meshes",
        "cluster_chunks",
        "cluster_chunks_spatial",
        "cluster_rejects",
    ];
    if mapping.is_some() {
        tables.extend([
            "cluster_instance_map",
            "cluster_reference_coverage",
            "cluster_provenance",
        ]);
    }
    for table in tables {
        transaction.execute(&format!("DELETE FROM {table}"), [])?;
    }
    transaction.execute(
        "INSERT INTO cluster_build(id, builder_identity, triangle_threshold, block_side_cells, lod_mode)
         VALUES (1, ?1, ?2, ?3, 'cluster')",
        params![identity_hex, triangle_threshold, BLOCK_TIER],
    )?;
    for outcome in outcomes {
        for prim in &outcome.prims {
            match &prim.status {
                PrimStatus::Cluster(mesh) => {
                    publish::insert_mesh_row(
                        &transaction,
                        &publish::MeshRow {
                            mesh_hash: &outcome.hash_hex,
                            primitive_index: prim.index,
                            payload_path: &mesh.payload_path,
                            content_hash: &mesh.content_hash,
                            source_triangles: mesh.source_triangles,
                            cluster_count: mesh.cluster_count,
                            vertex_count: mesh.vertex_count,
                            payload_bytes: mesh.payload_bytes,
                            material_key: &prim.material_key,
                            model_path: &outcome.glb_path,
                            material_index: prim.material_index,
                            alpha_mode: if prim.alpha == AlphaMode::Mask {
                                "mask"
                            } else {
                                "opaque"
                            },
                            alpha_cutoff: prim.alpha_cutoff,
                            double_sided: prim.double_sided,
                        },
                        true,
                    )?;
                }
                PrimStatus::Rejected { reason, detail } => {
                    transaction.execute(
                        "INSERT OR REPLACE INTO cluster_rejects(mesh_hash, primitive_index, reason, detail)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![outcome.hash_hex, prim.index, reason, detail],
                    )?;
                }
            }
        }
    }
    for chunk in chunks {
        transaction.execute(
            "INSERT INTO cluster_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash, input_key,
                 bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x, bounds_max_y, bounds_max_z,
                 instance_count, source_cells)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                chunk.worldspace_id,
                BLOCK_TIER,
                chunk.anchor.0,
                chunk.anchor.1,
                chunk.payload_path,
                chunk.content_hash,
                chunk.input_key,
                chunk.bounds.0[0],
                chunk.bounds.0[1],
                chunk.bounds.0[2],
                chunk.bounds.1[0],
                chunk.bounds.1[1],
                chunk.bounds.1[2],
                chunk.instance_count,
                chunk.source_cells,
            ],
        )?;
        transaction.execute(
            "INSERT INTO cluster_chunks_spatial(id, minX, maxX, minY, maxY, worldspace_id, tier, anchor_x, anchor_y)
             VALUES ((SELECT COALESCE(MAX(id), 0) + 1 FROM cluster_chunks_spatial), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                chunk.bounds.0[0],
                chunk.bounds.1[0],
                chunk.bounds.0[1],
                chunk.bounds.1[1],
                chunk.worldspace_id,
                BLOCK_TIER,
                chunk.anchor.0,
                chunk.anchor.1,
            ],
        )?;
    }
    if let Some(plan) = mapping {
        write_mapping(&transaction, plan, identity_hex, outcomes, chunks)?;
    }
    transaction.commit()?;
    Ok(())
}

/// Writes the mapping tables from the plan and the final blocks. A primitive the GLB header names
/// but the model pass did not produce (the model could not be loaded) is recorded as a reject, so
/// the expected set of every expanded reference is listed and accounted for.
fn write_mapping(
    transaction: &rusqlite::Transaction<'_>,
    plan: &MappingPlan,
    identity_hex: &str,
    outcomes: &[ModelOutcome],
    chunks: &[ChunkRow],
) -> Result<()> {
    transaction.execute(
        "INSERT INTO cluster_provenance(id, instance_map_version, coverage_version, digest_version,
             digest_algorithm, built_from, builder_identity, classifier_scope)
         VALUES (1, ?1, ?2, ?3, 'sha256', ?4, ?5, ?6)",
        params![
            INSTANCE_MAP_VERSION,
            COVERAGE_VERSION,
            DIGEST_VERSION,
            plan.built_from,
            identity_hex,
            plan.scope_json
        ],
    )?;
    // Per model: every primitive of its GLB, as clustered or rejected with the recorded reason.
    let mut model_rows: HashMap<&str, Vec<(u32, &'static str, String)>> = HashMap::new();
    for outcome in outcomes {
        let Some(expected) = plan.catalog.facts_by_hash(&outcome.hash_hex) else {
            continue;
        };
        let known: HashMap<u32, &PrimStatus> = outcome
            .prims
            .iter()
            .map(|prim| (prim.index, &prim.status))
            .collect();
        let mut rows = Vec::with_capacity(expected.len());
        for index in expected {
            match known.get(&index) {
                Some(PrimStatus::Cluster(_)) => rows.push((index, "clustered", String::new())),
                Some(PrimStatus::Rejected { reason, .. }) => {
                    rows.push((index, "rejected", reason.clone()));
                }
                _ => {
                    transaction.execute(
                        "INSERT OR REPLACE INTO cluster_rejects(mesh_hash, primitive_index, reason, detail)
                         VALUES (?1, ?2, 'model_load_failed', ?3)",
                        params![
                            outcome.hash_hex,
                            index,
                            outcome.load_failed.clone().unwrap_or_default()
                        ],
                    )?;
                    rows.push((index, "rejected", "model_load_failed".to_owned()));
                }
            }
        }
        model_rows.insert(outcome.hash_hex.as_str(), rows);
    }
    let mut coverage = transaction.prepare(
        "INSERT INTO cluster_reference_coverage(reference_id, placed_worldspace_id, source_cell_id,
             scope, model_hash, primitive_index, status, reason, input_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for seed in &plan.seeds {
        let mut common = |scope: &str, model: &str, primitive: i64, status: &str, reason: &str| {
            coverage.execute(params![
                seed.reference_id,
                seed.placed_worldspace_id,
                seed.source_cell_id,
                scope,
                model,
                primitive,
                status,
                reason,
                seed.digest
            ])
        };
        match &seed.eligibility {
            Eligibility::Eligible => {
                let model = seed.model.as_deref().ok_or_else(|| {
                    eyre!("eligible reference {} has no model", seed.reference_id)
                })?;
                let rows = model_rows.get(model).ok_or_else(|| {
                    eyre!(
                        "eligible reference {} names model {model}, which the stage did not process",
                        seed.reference_id
                    )
                })?;
                common("reference", model, -1, "expanded", "")?;
                for (index, status, reason) in rows {
                    common("primitive", model, i64::from(*index), status, reason)?;
                }
            }
            Eligibility::NotEligible(reason) => {
                common("reference", "", -1, "not_eligible", reason)?;
            }
            Eligibility::Unknown(reason) => {
                common("reference", "", -1, "unknown", reason)?;
            }
        }
    }
    drop(coverage);
    let mut insert = transaction.prepare(
        "INSERT INTO cluster_instance_map(worldspace_id, tier, anchor_x, anchor_y, chunk_content_hash,
             ordinal, reference_id, placed_worldspace_id, cell_x, cell_y, model_hash, primitive_index,
             input_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for chunk in chunks {
        for row in &chunk.map {
            insert.execute(params![
                chunk.worldspace_id,
                BLOCK_TIER,
                chunk.anchor.0,
                chunk.anchor.1,
                chunk.content_hash,
                row.ordinal,
                row.reference_id,
                row.placed_worldspace_id,
                row.cell.0,
                row.cell.1,
                row.model_hash,
                row.primitive_index,
                row.digest
            ])?;
        }
    }
    Ok(())
}
