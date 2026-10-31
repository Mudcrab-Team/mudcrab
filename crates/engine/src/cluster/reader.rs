//! One-time authoritative validation. Per-view drawing uses this immutable index.

use super::envelope::{self, ClusterEnvelope};
use bevy::math::IVec2;
use color_eyre::eyre::{Result, ensure, eyre};
use rusqlite::Connection;
#[cfg(test)]
use rusqlite::OpenFlags;
use serde::Deserialize;
use shared::{
    cluster::parse_hash,
    cluster_mapping::{
        AuthoritativeInputs, CoverageState, DIGEST_VERSION, ReferenceSnapshot,
        prepare_mapping_with_payloads, validate_prepared_mapping_parallel,
    },
    cluster_recipe::{
        authority::CoreAuthority,
        classify::spawned_model_path,
        scope::{SCOPE_VERSION, Scope},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

#[cfg(test)]
use shared::cluster_mapping::{validate_mapping, validate_mapping_parallel};
#[cfg(test)]
use std::cell::RefCell;

/// One open's stable authoritative inputs, under the envelope's retained shared source lock.
/// Cooperating writers must honor that guard: this does not make mutable inputs immutable.
/// The first reference evaluation still runs the delegate's full source/catalog checks; only
/// its owned snapshot (including None) is reused. No cache survives CertifiedPackage::open.
#[cfg(test)]
struct OpenAuthorityMemo<'a, A> {
    authority: &'a A,
    references: RefCell<BTreeMap<u32, Option<ReferenceSnapshot>>>,
}

#[cfg(test)]
impl<'a, A> OpenAuthorityMemo<'a, A> {
    fn new(authority: &'a A) -> Self {
        Self {
            authority,
            references: RefCell::new(BTreeMap::new()),
        }
    }
}

#[cfg(test)]
impl<A: AuthoritativeInputs> AuthoritativeInputs for OpenAuthorityMemo<'_, A> {
    fn core_build_identity(&self) -> Option<String> {
        self.authority.core_build_identity()
    }

    fn builder_identity(&self) -> String {
        self.authority.builder_identity()
    }

    fn reference(&self, reference_id: u32) -> Option<ReferenceSnapshot> {
        if let Some(snapshot) = self.references.borrow().get(&reference_id) {
            return snapshot.clone();
        }
        let snapshot = self.authority.reference(reference_id);
        self.references
            .borrow_mut()
            .insert(reference_id, snapshot.clone());
        snapshot
    }

    fn classifier_scope(&self) -> String {
        self.authority.classifier_scope()
    }

    fn lod_origin(&self, worldspace_id: u32) -> Option<(i32, i32)> {
        self.authority.lod_origin(worldspace_id)
    }

    fn model_primitives(&self, model_hash: &str) -> Option<BTreeSet<u32>> {
        self.authority.model_primitives(model_hash)
    }

    fn model_materials(
        &self,
        model_hash: &str,
    ) -> Option<BTreeMap<u32, shared::cluster_mapping::MaterialSource>> {
        self.authority.model_materials(model_hash)
    }

    fn examined_references(&self) -> BTreeSet<u32> {
        self.authority.examined_references()
    }
}

/// Local to one retained package/stamp: identical MCI bytes may be placed twice.
/// Never combine record keys from different package generations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordKey {
    pub worldspace: u32,
    pub tier: u32,
    pub anchor: [i32; 2],
    pub ordinal: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferencePrimitive {
    pub reference: u32,
    pub worldspace: u32,
    pub source_cell: u32,
    pub model_hash: String,
    pub primitive: u32,
}

/// Raw authoritative placement retained so stock binding can use the exact
/// same cell-local Transform construction as streaming, without numeric tolerances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferencePose {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
}

pub use shared::cluster_recipe::refusal::StockRefusalClass;

/// One examined reference's authoritative placement, including references with no mapped record.
/// Inventory membership and a known source path do not certify drawable or replacement coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedReference {
    pub is_tree: bool,
    /// Authoritative base record type is STAT or TREE, regardless of refusal reason.
    pub is_static_scenery: bool,
    /// None is missing base flag data. True still needs per-level MNAM paths.
    pub base_has_distant_lod: Option<bool>,
    /// The REFR record header's Visible When Distant flag (bit 15). Native object LOD holds a
    /// placement only when it is set: the same base appears with and without it.
    pub visible_when_distant: bool,
    pub snapshot: ReferenceSnapshot,
    pub pose: ReferencePose,
    pub model_path: Option<String>,
    pub coverage: CoverageState,
    pub refusal_class: StockRefusalClass,
    /// Exact rejected source ordinals for expanded Complete/Partial references. Unknown and
    /// NotEligible references have no certified expected primitive set, represented by None.
    pub stock_primitives: Option<BTreeSet<u32>>,
}

/// Describes validation of the retained package. Only the package guard authorizes use.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidationStamp {
    extension_sha256: String,
    core_identity: String,
    scope_version: u32,
    digest_version: u32,
    content_check: ContentCheck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentCheck {
    FullHash,
}

/// Only `open` constructs this. Coverage rows are a cache after authority validation,
/// never an authority of their own. This does not prove any view can draw a record.
pub struct CertifiedPackage {
    envelope: ClusterEnvelope,
    stamp: ValidationStamp,
    records: BTreeMap<RecordKey, ReferencePrimitive>,
    references: BTreeMap<u32, ReferenceSnapshot>,
    reference_paths: BTreeMap<u32, String>,
    reference_poses: BTreeMap<u32, ReferencePose>,
    placed_cells: BTreeMap<(u32, i32, i32), Vec<RecordKey>>,
    /// Validated expected primitive ordinals without a cluster representation,
    /// retained only for references that have at least one mapped record.
    unclustered_primitives: BTreeMap<u32, BTreeSet<u32>>,
    /// Separate from mapped-only replacement membership above; contains the full examined scope.
    placed_references: BTreeMap<u32, PlacedReference>,
    placed_reference_cells: BTreeMap<(u32, i32, i32), Vec<u32>>,
    /// Winning-record enable definitions, retained under the same dataset lock as placement.
    enable_catalog: std::sync::Arc<crate::world::reference_state::EnableCatalog>,
}

impl CertifiedPackage {
    /// Certifies `overlay` (a cluster overlay folder) against the base dataset `base`.
    pub fn open(base: &Path, overlay: &Path, terrain: bool) -> Result<Self> {
        Self::open_with_prefetch(base, overlay, terrain, |_, _| {})
    }

    /// The hook may start speculative source decoding under the retained dataset lock. It
    /// grants no authority: certification still reads all source bytes and can refuse later.
    pub(crate) fn open_with_prefetch(
        base: &Path,
        overlay: &Path,
        terrain: bool,
        prefetch: impl FnOnce(&Connection, &[(String, String)]),
    ) -> Result<Self> {
        bevy::log::debug!("checking cluster extension ownership and payloads");
        let envelope = envelope::open(base, overlay, terrain)?;
        bevy::log::debug!("cluster payload checks passed; opening the source authority");
        // The base with the overlay attached read-only: cluster tables and core tables, one view.
        let connection = envelope.connection()?;
        let enable_catalog = std::sync::Arc::new(
            crate::world::reference_state::EnableCatalog::read(&connection)?,
        );
        let declared: String = connection.query_row(
            "SELECT classifier_scope FROM cluster_provenance WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let scope = Scope::from_declared_json(&declared)
            .map_err(|reason| eyre!("unsupported cluster classifier scope: {reason}"))?;
        // The builder is an opaque converter identity. The runtime verifies consistency;
        // it cannot recompute the converter's source-code hash from an engine binary.
        let manifest_bytes = std::fs::read(envelope.root.join("conversion-manifest.json"))?;
        let published = published_files(&manifest_bytes)?;
        drop(manifest_bytes);
        prefetch(&connection, &published);
        // Retain the exact reference-resolved source path, not merely a GLB
        // content hash: equal bytes in different folders can resolve materials
        // differently. Use the same shared classifier recipe over this core DB.
        let mut core_authority = CoreAuthority::open_declared(
            // The authority reads the core only, through a connection with nothing attached.
            envelope::open_base_certification_connection(&envelope.root)?,
            &envelope.root,
            published,
            scope,
            envelope.builder_identity.clone(),
        )?;
        bevy::log::debug!("checking cluster mapping against authoritative source inputs");
        // The main thread performs serial structure checks while the source worker reserves
        // one additional CPU for it. Both results join under envelope's retained source lock.
        let overlap = std::thread::available_parallelism().is_ok_and(|cpus| cpus.get() > 3);
        let prepared = prepare_after_snapshots(
            |busy| {
                core_authority.preload_snapshots_with_headroom(busy)?;
                Ok(())
            },
            || {
                Ok(prepare_mapping_with_payloads(
                    &connection,
                    &envelope.overlay,
                    Some(&envelope.core_identity),
                    &envelope.payloads()?,
                )?)
            },
            overlap,
        )?;
        let authority = core_authority.preloaded_authority()?;
        let validated = validate_prepared_mapping_parallel(prepared, &authority)?;
        bevy::log::debug!("cluster source checks passed; indexing placed references");
        // Reuse only the records that just passed every structure/source check in this open.
        // Neither decoded rows nor successful digests survive as trust into another launch.
        let runtime_coverage = validated.coverage;
        let mut records = BTreeMap::new();
        let mut ids = BTreeSet::new();
        for instance in validated.instances {
            let key = RecordKey {
                worldspace: instance.block.0,
                tier: instance.block.1,
                anchor: [instance.block.2, instance.block.3],
                ordinal: instance.ordinal,
            };
            let identity = ReferencePrimitive {
                reference: instance.reference_id,
                worldspace: instance.placed_worldspace_id,
                source_cell: 0, // filled from the validated snapshot below
                model_hash: instance.model_hash,
                primitive: instance.primitive_index,
            };
            ids.insert(identity.reference);
            ensure!(
                records.insert(key, identity).is_none(),
                "duplicate certified record context"
            );
        }
        bevy::log::info!(
            phase = "mapped_records_reuse",
            records = records.len(),
            coverage_projection_reused = runtime_coverage.is_some(),
            "cluster certification subphase"
        );
        let inputs = ids
            .into_iter()
            .map(|id| {
                core_authority
                    .preloaded_reference(id)
                    .map(|resolved| (id, resolved))
                    .ok_or_else(|| eyre!("certified reference disappeared"))
            })
            .collect::<Result<Vec<_>>>()?;
        let facts = index_reference_facts(&inputs, &authority, true)?;
        let mut references = BTreeMap::new();
        let mut reference_paths = BTreeMap::new();
        let mut reference_poses = BTreeMap::new();
        for (id, snapshot, path, pose) in facts {
            references.insert(id, snapshot);
            reference_paths.insert(id, path);
            reference_poses.insert(id, pose);
        }
        for identity in records.values_mut() {
            identity.source_cell = references[&identity.reference].source_cell_id;
        }
        bevy::log::info!(
            phase = "mapped_reference_index",
            records = records.len(),
            references = references.len(),
            "cluster certification phase"
        );
        // Keep the full validated scope independently of mapped replacement candidates. Reuse
        // mapped references' source facts; resolve only the additional, unmapped references.
        let mut placed_references = BTreeMap::new();
        let mut placed_reference_cells = BTreeMap::<_, Vec<_>>::new();
        // Atypical coverage schemas/rows retain the original runtime SQL and its errors.
        let coverage_states = runtime_coverage
            .is_none()
            .then(|| read_coverage_states(&connection))
            .transpose()?;
        let refusal_classes = read_stock_refusal_classes(&connection)?;
        for reference in authority.examined_references() {
            let resolved = core_authority
                .preloaded_reference(reference)
                .ok_or_else(|| eyre!("examined reference disappeared"))?;
            let (snapshot, pose, model_path) = if let Some(snapshot) = references.get(&reference) {
                (
                    snapshot.clone(),
                    reference_poses[&reference],
                    Some(reference_paths[&reference].clone()),
                )
            } else {
                (
                    authority
                        .reference(reference)
                        .ok_or_else(|| eyre!("examined reference disappeared"))?,
                    ReferencePose {
                        position: resolved.position,
                        rotation: resolved.rotation,
                        scale: resolved.scale,
                    },
                    spawned_model_path(resolved.model_path.as_deref(), resolved.base_is_marker),
                )
            };
            let coverage = runtime_coverage
                .as_ref()
                .and_then(|rows| rows.get(&reference).map(|row| row.state))
                .or_else(|| {
                    coverage_states
                        .as_ref()
                        .and_then(|states| states.get(&reference).copied())
                })
                .unwrap_or(CoverageState::Absent);
            let stock_primitives = match coverage {
                CoverageState::Complete | CoverageState::Partial => Some(BTreeSet::new()),
                CoverageState::NotEligible | CoverageState::Unknown => None,
                CoverageState::Absent => return Err(eyre!("examined reference lost coverage")),
            };
            placed_reference_cells
                .entry((
                    snapshot.placed_worldspace_id,
                    snapshot.cell.0,
                    snapshot.cell.1,
                ))
                .or_default()
                .push(reference);
            placed_references.insert(
                reference,
                PlacedReference {
                    // The REFR header's Visible When Distant (bit 15); the base's bit 15
                    // (Has Distant LOD) is base_has_distant_lod.
                    visible_when_distant: resolved.header_flags & (1 << 15) != 0,
                    snapshot,
                    pose,
                    model_path,
                    coverage,
                    refusal_class: refusal_classes
                        .get(&reference)
                        .copied()
                        .unwrap_or(StockRefusalClass::Other),
                    is_tree: resolved.is_tree,
                    is_static_scenery: matches!(resolved.record_type.as_str(), "STAT" | "TREE"),
                    base_has_distant_lod: resolved.base_has_distant_lod,
                    stock_primitives,
                },
            );
        }
        bevy::log::info!(
            phase = "examined_reference_index",
            references = placed_references.len(),
            cells = placed_reference_cells.len(),
            "cluster certification phase"
        );
        // validate_mapping has already proved the exact source enumeration and
        // its status rows. Retain rejected pieces too: mapped records alone can
        // never certify a partial reference while stock is still uninstantiated.
        let mut unclustered_primitives: BTreeMap<_, BTreeSet<u32>> = references
            .keys()
            .map(|reference| (*reference, BTreeSet::new()))
            .collect();
        let mut insert_unclustered = |reference, primitive| {
            if let Some(expected) = unclustered_primitives.get_mut(&reference) {
                expected.insert(primitive);
            }
            if let Some(expected) = placed_references
                .get_mut(&reference)
                .and_then(|placed| placed.stock_primitives.as_mut())
            {
                expected.insert(primitive);
            }
        };
        if let Some(rows) = &runtime_coverage {
            for (&reference, row) in rows {
                for &primitive in &row.unclustered_primitives {
                    insert_unclustered(reference, primitive);
                }
            }
        } else {
            let mut coverage = connection.prepare(
                "SELECT reference_id, primitive_index FROM cluster_reference_coverage
             WHERE scope = 'primitive' AND status <> 'clustered'",
            )?;
            for row in
                coverage.query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)))?
            {
                let (reference, primitive) = row?;
                insert_unclustered(reference, primitive);
            }
        }
        // Exterior streaming selects references by position, including persistent
        // references authored under a different owning CELL. Use the same placed
        // grid from the authoritative recipe, never source_cell_id or block grid.
        let mut placed_cells = BTreeMap::<_, Vec<_>>::new();
        for (key, record) in &records {
            let snapshot = &references[&record.reference];
            placed_cells
                .entry((record.worldspace, snapshot.cell.0, snapshot.cell.1))
                .or_default()
                .push(*key);
        }
        drop(runtime_coverage);
        let stamp = ValidationStamp {
            extension_sha256: envelope.base.sha256.clone(),
            core_identity: envelope.core_identity.clone(),
            scope_version: SCOPE_VERSION,
            digest_version: DIGEST_VERSION,
            content_check: ContentCheck::FullHash,
        };
        bevy::log::info!(
            phase = "coverage_and_cell_index",
            cells = placed_cells.len(),
            "cluster certification phase"
        );
        bevy::log::debug!(
            references = references.len(),
            records = records.len(),
            "cluster source certificate ready; per-view drawing remains unproved"
        );
        Ok(Self {
            envelope,
            stamp,
            records,
            references,
            reference_paths,
            reference_poses,
            placed_cells,
            unclustered_primitives,
            placed_references,
            placed_reference_cells,
            enable_catalog,
        })
    }

    pub fn envelope(&self) -> &ClusterEnvelope {
        &self.envelope
    }
    pub fn stamp(&self) -> &ValidationStamp {
        &self.stamp
    }
    /// The authoritative placement allows eligibility to be revoked if runtime motion changes it.
    pub fn reference(&self, id: u32) -> Option<&ReferenceSnapshot> {
        self.references.get(&id)
    }
    /// The exact source path resolved for this certified reference. A stock
    /// scene must match this path AND its certified primitive/model identity;
    /// equal GLB bytes at another path alone cannot prove material equivalence.
    pub fn reference_model_path(&self, reference: u32) -> Option<&str> {
        self.reference_paths.get(&reference).map(String::as_str)
    }
    pub fn reference_pose(&self, reference: u32) -> Option<&ReferencePose> {
        self.reference_poses.get(&reference)
    }
    pub fn record(&self, key: &RecordKey) -> Option<&ReferencePrimitive> {
        self.records.get(key)
    }
    /// Authoritative examined-scope inventory, independent of mapped replacement membership.
    /// None means absent from this declared scope, not known empty or eligible.
    pub fn placed_reference(&self, id: u32) -> Option<&PlacedReference> {
        self.placed_references.get(&id)
    }
    pub(crate) fn enable_catalog(
        &self,
    ) -> &std::sync::Arc<crate::world::reference_state::EnableCatalog> {
        &self.enable_catalog
    }
    /// Nonempty position cells in exactly this placing world, in grid order.
    /// None selects the whole worldspace, matching owned clusters' unbounded object reach.
    pub(crate) fn placed_cells_in_window(
        &self,
        world: u32,
        center: IVec2,
        radius: Option<i32>,
    ) -> impl Iterator<Item = (IVec2, &[u32])> {
        let (low, high) = radius.map_or((IVec2::splat(i32::MIN), IVec2::splat(i32::MAX)), |r| {
            let r = r.max(0);
            (
                IVec2::new(center.x.saturating_sub(r), center.y.saturating_sub(r)),
                IVec2::new(center.x.saturating_add(r), center.y.saturating_add(r)),
            )
        });
        // Walk indexed cells, not every possible x coordinate of an unbounded/sparse world.
        self.placed_reference_cells
            .range((world, low.x, i32::MIN)..=(world, high.x, i32::MAX))
            .filter(move |((_, _, y), _)| {
                radius.is_none_or(|r| r >= 0) && *y >= low.y && *y <= high.y
            })
            .map(|((_, x, y), ids)| (IVec2::new(*x, *y), ids.as_slice()))
    }

    /// Every examined reference placed in this position cell of this placing worldspace,
    /// including refused and unknown references. Owning CELL and block world may differ.
    pub fn placed_references_in_cell(
        &self,
        world: u32,
        grid: IVec2,
    ) -> impl Iterator<Item = (u32, &PlacedReference)> {
        self.placed_reference_cells
            .get(&(world, grid.x, grid.y))
            .into_iter()
            .flat_map(|references| references.iter())
            .map(|reference| (*reference, &self.placed_references[reference]))
    }
    /// Position-based exterior streaming membership, distinct from the authored owning CELL.
    pub fn records_in_placed_cell(
        &self,
        world: u32,
        grid: IVec2,
    ) -> impl Iterator<Item = (&RecordKey, &ReferencePrimitive)> {
        self.placed_cells
            .get(&(world, grid.x, grid.y))
            .into_iter()
            .flat_map(|keys| keys.iter())
            .map(|key| (key, &self.records[key]))
    }

    /// True only for a mapped reference with no validated expected stock-only pieces.
    pub fn reference_complete(&self, reference: u32) -> bool {
        self.unclustered_primitives
            .get(&reference)
            .is_some_and(BTreeSet::is_empty)
    }
    /// Expected source ordinals that must exist on stock before partial replacement
    /// can satisfy handoff. None means this certificate has no mapped reference.
    pub fn unclustered_primitives(&self, reference: u32) -> Option<&BTreeSet<u32>> {
        self.unclustered_primitives.get(&reference)
    }

    pub fn records(&self) -> impl Iterator<Item = (&RecordKey, &ReferencePrimitive)> {
        self.records.iter()
    }
}

#[derive(Deserialize)]
struct PublishedManifest {
    entries: BTreeMap<String, PublishedFile>,
}
#[derive(Deserialize)]
struct PublishedFile {
    output: String,
    output_hash: String,
}

type IndexedReference = (u32, ReferenceSnapshot, String, ReferencePose);

/// Structure reads and model/snapshot preparation use independent connections under the same
/// retained dataset lock. Join before selecting either result: snapshot errors retain their
/// original priority, and no prepared structure by itself authorizes runtime use.
fn prepare_after_snapshots<M>(
    snapshots: impl FnOnce(usize) -> Result<()> + Send,
    mapping: impl FnOnce() -> Result<M>,
    overlap: bool,
) -> Result<M> {
    if !overlap {
        snapshots(0)?;
        return mapping();
    }
    std::thread::scope(|scope| {
        let snapshots = std::thread::Builder::new()
            .name("cluster-snapshot-preparation".into())
            .spawn_scoped(scope, || snapshots(1))?;
        let mapping = mapping();
        snapshots
            .join()
            .map_err(|_| eyre!("cluster snapshot preparation worker panicked"))??;
        mapping
    })
}

/// Partition by reference, then merge in reference order regardless of task completion order.
/// Only decoded rows and immutable authority facts cross the compute-pool boundary.
fn index_reference_facts(
    inputs: &[(u32, &shared::cluster_recipe::classify::ScopedReference)],
    authority: &(dyn AuthoritativeInputs + Sync),
    parallel: bool,
) -> Result<Vec<IndexedReference>> {
    let index = |&(id, resolved): &(u32, &shared::cluster_recipe::classify::ScopedReference)| {
        let snapshot = authority
            .reference(id)
            .ok_or_else(|| eyre!("certified reference disappeared"))?;
        let path = spawned_model_path(resolved.model_path.as_deref(), resolved.base_is_marker)
            .ok_or_else(|| eyre!("certified reference has no resolved model path"))?;
        Ok((
            id,
            snapshot,
            path,
            ReferencePose {
                position: resolved.position,
                rotation: resolved.rotation,
                scale: resolved.scale,
            },
        ))
    };
    if !parallel || inputs.is_empty() {
        return inputs.iter().map(index).collect();
    }
    let pool = bevy::tasks::ComputeTaskPool::get_or_init(Default::default);
    let chunk_size = inputs.len().div_ceil(pool.thread_num().max(1));
    let mut parts = pool.scope(|scope| {
        for (ordinal, chunk) in inputs.chunks(chunk_size).enumerate() {
            let index = &index;
            scope.spawn(
                async move { (ordinal, chunk.iter().map(index).collect::<Result<Vec<_>>>()) },
            );
        }
    });
    parts.sort_unstable_by_key(|(ordinal, _)| *ordinal);
    let mut result = Vec::with_capacity(inputs.len());
    for (_, part) in parts {
        result.extend(part?);
    }
    Ok(result)
}

/// The same state recipe as coverage_state, evaluated once for the whole store.
/// Only used after validate_mapping has checked every state/primitive row.
fn read_coverage_states(connection: &Connection) -> rusqlite::Result<BTreeMap<u32, CoverageState>> {
    let mut statement = connection.prepare(
        "SELECT r.reference_id, r.status, p.rows, p.clustered
         FROM cluster_reference_coverage r
         LEFT JOIN (
             SELECT reference_id, COUNT(*) AS rows, SUM(status = 'clustered') AS clustered
             FROM cluster_reference_coverage WHERE scope = 'primitive' GROUP BY reference_id
         ) p ON p.reference_id = r.reference_id
         WHERE r.scope = 'reference' ORDER BY r.reference_id",
    )?;
    let rows = statement.query_map([], |row| {
        let status: String = row.get(1)?;
        let state = match status.as_str() {
            "not_eligible" => CoverageState::NotEligible,
            "unknown" => CoverageState::Unknown,
            _ => {
                let count = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
                let clustered = row.get::<_, Option<i64>>(3)?.unwrap_or(0);
                if count > 0 && clustered == count {
                    CoverageState::Complete
                } else {
                    CoverageState::Partial
                }
            }
        };
        Ok((row.get(0)?, state))
    })?;
    let mut states = BTreeMap::new();
    for row in rows {
        let (reference, state) = row?;
        // Match query_row's first result even if a malformed store lacks the
        // unique reference-state index (the normal schema forbids duplicates).
        states.entry(reference).or_insert(state);
    }
    Ok(states)
}

/// Project validated coverage through the producer's shared refusal contract.
fn read_stock_refusal_classes(
    connection: &Connection,
) -> rusqlite::Result<BTreeMap<u32, StockRefusalClass>> {
    let mut statement = connection.prepare(
        "SELECT reference_id, status, reason FROM cluster_reference_coverage
         WHERE scope = 'reference' OR (scope = 'primitive' AND status = 'rejected')",
    )?;
    let mut classes = BTreeMap::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let reference: u32 = row.get(0)?;
        let status: String = row.get(1)?;
        let reason: Option<String> = row.get(2)?;
        let class =
            shared::cluster_recipe::refusal::stock_refusal_class(&status, reason.as_deref());
        let current = classes.entry(reference).or_insert(StockRefusalClass::None);
        *current = (*current).max(class);
    }
    Ok(classes)
}

fn published_files(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let manifest: PublishedManifest = serde_json::from_slice(bytes)?;
    let mut files = BTreeMap::new();
    for entry in manifest.entries.into_values() {
        let path = &entry.output;
        ensure!(
            !path.is_empty()
                && !path.contains(['\\', ':', '\0'])
                && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
                && Path::new(path)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "unsafe published asset path"
        );
        ensure!(
            parse_hash(&entry.output_hash).is_some(),
            "invalid published asset hash"
        );
        if let Some(old) = files.insert(entry.output, entry.output_hash.clone()) {
            ensure!(
                old == entry.output_hash,
                "conflicting published asset identities"
            );
        }
    }
    Ok(files.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapped_preparation_joins_and_preserves_snapshot_refusal_priority() {
        use std::sync::{
            Barrier,
            atomic::{AtomicBool, Ordering},
        };
        let barrier = Barrier::new(2);
        let finished = AtomicBool::new(false);
        let result: Result<()> = prepare_after_snapshots(
            |busy| {
                assert_eq!(busy, 1);
                barrier.wait();
                finished.store(true, Ordering::Release);
                Err(eyre!("snapshot refusal"))
            },
            || {
                barrier.wait();
                Err(eyre!("mapping refusal"))
            },
            true,
        );
        assert!(finished.load(Ordering::Acquire));
        assert_eq!(result.unwrap_err().to_string(), "snapshot refusal");
        let result: Result<()> =
            prepare_after_snapshots(|_| Ok(()), || Err(eyre!("mapping refusal")), true);
        assert_eq!(result.unwrap_err().to_string(), "mapping refusal");
    }

    #[test]
    fn serial_preparation_does_not_read_mapping_after_snapshot_refusal() {
        let result: Result<()> = prepare_after_snapshots(
            |busy| {
                assert_eq!(busy, 0);
                Err(eyre!("snapshot refusal"))
            },
            || panic!("mapping must retain its original later position"),
            false,
        );
        assert_eq!(result.unwrap_err().to_string(), "snapshot refusal");
    }

    #[test]
    fn set_based_coverage_matches_per_row_including_empty_expansion_and_absence() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE cluster_reference_coverage(reference_id INTEGER, scope TEXT, status TEXT);
            INSERT INTO cluster_reference_coverage VALUES
            (1,'reference','expanded'),
            (2,'reference','expanded'),(2,'primitive','clustered'),
            (3,'reference','expanded'),(3,'primitive','clustered'),(3,'primitive','rejected'),
            (3,'reference','unknown'),
            (4,'reference','unknown'),(4,'primitive','clustered'),
            (5,'reference','not_eligible'),(5,'primitive','clustered');").unwrap();
        let states = read_coverage_states(&db).unwrap();
        for reference in 1..=6 {
            assert_eq!(
                states
                    .get(&reference)
                    .copied()
                    .unwrap_or(CoverageState::Absent),
                shared::cluster_mapping::coverage_state(&db, reference).unwrap(),
            );
        }
        assert_eq!(states[&1], CoverageState::Partial);
        assert_eq!(states[&2], CoverageState::Complete);
        assert_eq!(states[&3], CoverageState::Partial);
        assert_eq!(states[&4], CoverageState::Unknown);
        assert_eq!(states[&5], CoverageState::NotEligible);
    }

    fn refusal_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE cluster_reference_coverage(
                 reference_id INTEGER, scope TEXT, status TEXT, reason TEXT)",
        )
        .unwrap();
        db
    }

    #[test]
    fn stock_refusal_projection_recognizes_every_exact_reason_in_both_scopes() {
        use StockRefusalClass::{Look, Other, Runtime};
        let db = refusal_db();
        for (reason, expected) in [
            ("not_opaque", Look),
            ("unsupported_material_feature", Look),
            ("unresolved_reference_override:XEMI", Look),
            ("unresolved_reference_override:XALP", Look),
            ("missing_attributes", Look),
            ("effect_shader", Look),
            ("non_rendering", Look),
            ("state_change_writer_unavailable", Runtime),
            ("scripted", Runtime),
            ("enable_parent", Runtime),
            ("initially_disabled", Runtime),
            ("unresolved_reference_override:OTHER", Other),
            ("not_opaque:detail", Other),
            ("NOT_OPAQUE", Other),
            ("future_reason", Other),
        ] {
            for (scope, status) in [("reference", "not_eligible"), ("primitive", "rejected")] {
                db.execute("DELETE FROM cluster_reference_coverage", [])
                    .unwrap();
                db.execute(
                    "INSERT INTO cluster_reference_coverage VALUES(1,?1,?2,?3)",
                    rusqlite::params![scope, status, reason],
                )
                .unwrap();
                assert_eq!(
                    read_stock_refusal_classes(&db).unwrap()[&1],
                    expected,
                    "{scope}: {reason}"
                );
            }
        }
    }

    #[test]
    fn stock_refusal_projection_prioritizes_runtime_then_other_independent_of_row_order() {
        use StockRefusalClass::{Other, Runtime};
        let db = refusal_db();
        for (first, second, expected) in [
            ("initially_disabled", "not_opaque", Runtime),
            ("not_opaque", "initially_disabled", Runtime),
            ("future_reason", "not_opaque", Other),
            ("not_opaque", "future_reason", Other),
            ("scripted", "future_reason", Runtime),
            ("future_reason", "scripted", Runtime),
        ] {
            db.execute("DELETE FROM cluster_reference_coverage", [])
                .unwrap();
            db.execute(
                "INSERT INTO cluster_reference_coverage VALUES
                 (1,'reference','not_eligible',?1),(1,'primitive','rejected',?2)",
                [first, second],
            )
            .unwrap();
            assert_eq!(read_stock_refusal_classes(&db).unwrap()[&1], expected);
        }
    }

    #[test]
    fn stock_refusal_projection_treats_missing_refusal_reasons_as_other_but_success_as_none() {
        use StockRefusalClass::{Look, None, Other};
        let db = refusal_db();
        db.execute_batch(
            "INSERT INTO cluster_reference_coverage VALUES
             (1,'reference','not_eligible',NULL),
             (2,'reference','unknown',NULL),
             (3,'reference','expanded',NULL),(3,'primitive','rejected',NULL),
             (4,'reference','expanded',NULL),(4,'primitive','clustered',NULL),
             (5,'reference','expanded',''),(5,'primitive','clustered',''),
             (6,'reference','expanded',NULL),(6,'primitive','rejected','not_opaque'),
             (7,'reference','not_eligible',''),(8,'reference','unknown',''),
             (9,'reference','expanded',''),(9,'primitive','rejected',''),
             (10,'reference','expanded',NULL),(10,'primitive','clustered','scripted'),
             (11,'reference','not_eligible',NULL),(11,'primitive','rejected','not_opaque');",
        )
        .unwrap();
        let classes = read_stock_refusal_classes(&db).unwrap();
        for reference in [1, 2, 3, 7, 8, 9, 11] {
            assert_eq!(classes[&reference], Other, "{reference}");
        }
        for reference in [4, 5, 10] {
            assert_eq!(classes[&reference], None, "{reference}");
        }
        assert_eq!(classes[&6], Look);
        assert!(!classes.contains_key(&12));
        assert_eq!(StockRefusalClass::default(), None);
    }

    use serde_json::json;
    use shared::cluster_mapping::{Eligibility, FixedAuthority, MaterialSource};

    #[derive(Default)]
    struct CountingAuthority {
        inputs: FixedAuthority,
        reference_calls: RefCell<BTreeMap<u32, usize>>,
        other_calls: RefCell<BTreeMap<&'static str, usize>>,
    }

    impl CountingAuthority {
        fn forwarded(&self, method: &'static str) {
            *self.other_calls.borrow_mut().entry(method).or_default() += 1;
        }
    }

    impl AuthoritativeInputs for CountingAuthority {
        fn core_build_identity(&self) -> Option<String> {
            self.forwarded("core_build_identity");
            self.inputs.core_build_identity()
        }

        fn builder_identity(&self) -> String {
            self.forwarded("builder_identity");
            self.inputs.builder_identity()
        }

        fn reference(&self, reference_id: u32) -> Option<ReferenceSnapshot> {
            *self
                .reference_calls
                .borrow_mut()
                .entry(reference_id)
                .or_default() += 1;
            self.inputs.reference(reference_id)
        }

        fn classifier_scope(&self) -> String {
            self.forwarded("classifier_scope");
            self.inputs.classifier_scope()
        }

        fn lod_origin(&self, worldspace_id: u32) -> Option<(i32, i32)> {
            self.forwarded("lod_origin");
            self.inputs.lod_origin(worldspace_id)
        }

        fn model_primitives(&self, model_hash: &str) -> Option<BTreeSet<u32>> {
            self.forwarded("model_primitives");
            self.inputs.model_primitives(model_hash)
        }

        fn model_materials(&self, model_hash: &str) -> Option<BTreeMap<u32, MaterialSource>> {
            self.forwarded("model_materials");
            self.inputs.model_materials(model_hash)
        }

        fn examined_references(&self) -> BTreeSet<u32> {
            self.forwarded("examined_references");
            self.inputs.examined_references()
        }
    }

    fn memo_snapshot(world: u32) -> ReferenceSnapshot {
        ReferenceSnapshot {
            base_form_id: 17,
            snow_material: 0,
            placed_worldspace_id: world,
            source_cell_id: 23,
            cell: (-3, 5),
            block: (world, 16, -1, 0),
            digest: "ab".repeat(32),
            model: None,
            transform: None,
            eligibility: Eligibility::Unknown("invented_missing_input".into()),
        }
    }

    #[test]
    fn open_authority_memo_caches_present_and_absent_references_only_for_its_lifetime() {
        let mut authority = CountingAuthority::default();
        authority.inputs.references.insert(1, memo_snapshot(7));
        authority.inputs.references.insert(2, memo_snapshot(8));
        {
            let memo = OpenAuthorityMemo::new(&authority);
            for id in [1, 3, 1, 2, 3, 2] {
                assert_eq!(memo.reference(id), authority.inputs.reference(id));
            }
            assert_eq!(
                *authority.reference_calls.borrow(),
                BTreeMap::from([(1, 1), (2, 1), (3, 1)])
            );
            // The trait returns owned snapshots; caller mutation must not corrupt a later hit.
            let mut returned = memo.reference(1).unwrap();
            returned.cell = (99, -99);
            returned.digest.clear();
            assert_eq!(memo.reference(1), authority.inputs.reference(1));
            assert_eq!(authority.reference_calls.borrow()[&1], 1);
        }
        // Inputs may change between opens. A fresh wrapper must re-evaluate both previous
        // positive entries and previous explicit None results through the same delegate.
        authority.inputs.references.insert(1, memo_snapshot(9));
        authority.inputs.references.insert(3, memo_snapshot(10));
        let next_open = OpenAuthorityMemo::new(&authority);
        assert_eq!(next_open.reference(1), Some(memo_snapshot(9)));
        assert_eq!(next_open.reference(3), Some(memo_snapshot(10)));
        assert_eq!(
            *authority.reference_calls.borrow(),
            BTreeMap::from([(1, 2), (2, 1), (3, 2)])
        );
    }

    #[test]
    fn open_authority_memo_forwards_every_other_method_on_every_call() {
        let mut authority = CountingAuthority {
            inputs: FixedAuthority {
                core_build_identity: Some("invented-core".into()),
                builder_identity: "invented-builder".into(),
                classifier_scope: "invented-scope".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        authority.inputs.references.insert(1, memo_snapshot(7));
        authority.inputs.lod_origins.insert(7, (-32, 64));
        let model = "cd".repeat(32);
        authority
            .inputs
            .models
            .insert(model.clone(), BTreeSet::from([0, 2]));
        authority.inputs.materials.insert(
            model.clone(),
            BTreeMap::from([(
                2,
                MaterialSource {
                    model_path: "meshes/invented.glb".into(),
                    material_index: Some(3),
                    material_key: "invented-material".into(),
                    alpha_mask: true,
                    alpha_cutoff: Some(0.5),
                    double_sided: true,
                },
            )]),
        );
        let memo = OpenAuthorityMemo::new(&authority);
        for _ in 0..2 {
            assert_eq!(
                memo.core_build_identity(),
                authority.inputs.core_build_identity()
            );
            assert_eq!(memo.builder_identity(), authority.inputs.builder_identity());
            assert_eq!(memo.classifier_scope(), authority.inputs.classifier_scope());
            assert_eq!(memo.lod_origin(7), authority.inputs.lod_origin(7));
            assert_eq!(
                memo.model_primitives(&model),
                authority.inputs.model_primitives(&model)
            );
            assert_eq!(
                memo.model_materials(&model),
                authority.inputs.model_materials(&model)
            );
            assert_eq!(
                memo.examined_references(),
                authority.inputs.examined_references()
            );
        }
        assert_eq!(
            *authority.other_calls.borrow(),
            BTreeMap::from([
                ("core_build_identity", 2),
                ("builder_identity", 2),
                ("classifier_scope", 2),
                ("lod_origin", 2),
                ("model_primitives", 2),
                ("model_materials", 2),
                ("examined_references", 2),
            ])
        );
        assert!(authority.reference_calls.borrow().is_empty());
    }

    #[test]
    fn published_inputs_preserve_paths_and_refuse_conflicting_or_escaping_entries() {
        let h = "ab".repeat(32);
        let document = |entries| serde_json::to_vec(&json!({"entries":entries})).unwrap();
        let pair = |path: &str, hash: &str| json!({"output":path,"output_hash":hash});
        let valid = document(json!({"a":pair("meshes/a.glb",&h),"b":pair("textures/a.ktx2",&h)}));
        assert_eq!(
            published_files(&valid).unwrap(),
            vec![
                ("meshes/a.glb".into(), h.clone()),
                ("textures/a.ktx2".into(), h.clone())
            ]
        );
        for path in [
            "../a.glb",
            "/a.glb",
            "meshes/./a.glb",
            "meshes\\a.glb",
            "C:a.glb",
        ] {
            assert!(published_files(&document(json!({"a":pair(path,&h)}))).is_err());
        }
        assert!(
            published_files(&document(
                json!({"a":pair("meshes/a.glb",&h),"b":pair("meshes/a.glb",&"cd".repeat(32))})
            ))
            .is_err()
        );
    }
}

// Append as an additional inline module in cluster/reader.rs.
#[cfg(test)]
pub(crate) mod producer_fixture_tests {
    use super::*;
    use converter::{
        cache::{CacheEntry, ConversionManifest, hash_bytes},
        cluster::{
            LodMode,
            authority::CoreAuthority,
            probe::ProbeResult,
            publish::{PublishContext, finalize_base},
            stage::{StageSettings, run_stage},
        },
        esm::{exporter::create_tables, extractors::serialize_subrecords},
    };
    use rusqlite::params;
    use serde_json::{Value, json};
    use shared::{
        asset_lock::AssetLock,
        cluster::{NO_GROUP, hex, read_mci, read_mcl},
        cluster_mapping::{CoverageState, Eligibility, coverage_state},
    };
    use std::{collections::BTreeSet, fs};

    const WORLD: u32 = 0x70;
    const CHILD: u32 = 0x71;
    const PARENT_REF: u32 = 0x100;
    const CHILD_REF: u32 = 0x101;
    const PARENT_CELL: u32 = 0x30;
    const CHILD_CELL: u32 = 0x31;
    const MODEL: &str = "meshes/arch/rock.glb";
    const TEXTURE: &str = "textures/rock.ktx2";
    const BLEND_REF: u32 = 0x102;
    const GUARDED_REF: u32 = 0x103;
    const MISSING_MODEL_REF: u32 = 0x104;
    const UNPARSED_REF: u32 = 0x105;
    const TREE_REF: u32 = 0x106;
    const INTERIOR_REF: u32 = 0x107;
    const NO_MODEL_REF: u32 = 0x108;
    const RUNTIME_REF: u32 = 0x109;
    const INTERIOR_CELL: u32 = 0x32;
    const BLEND_MODEL: &str = "meshes/arch/blend.glb";
    const MISSING_MODEL: &str = "meshes/arch/missing.glb";
    const INVENTORY_GRID: IVec2 = IVec2::new(-2, 1);

    fn inventory_pose() -> ReferencePose {
        let side = shared::cluster_recipe::classify::CELL_SIZE;
        ReferencePose {
            position: [
                INVENTORY_GRID.x as f32 * side + 123.0,
                INVENTORY_GRID.y as f32 * side + 456.0,
                10.0,
            ],
            rotation: [0.1, -0.2, 0.3],
            scale: 1.25,
        }
    }

    fn glb(document: &Value, binary: &[u8]) -> Vec<u8> {
        let mut json = serde_json::to_vec(document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bin = binary.to_vec();
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let total = 12 + 8 + json.len() + 8 + bin.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    /// A 4x4 vertex grid (18 triangles) with normals, UVs and indices; with `broken` the model gets a
    /// second primitive that has only POSITION, which the stage rejects.
    fn grid_glb(broken: bool, uri: Option<&str>, blend: bool) -> Vec<u8> {
        grid_glb_with_billboard(broken, uri, blend, false)
    }

    fn grid_glb_with_billboard(
        broken: bool,
        uri: Option<&str>,
        blend: bool,
        billboard: bool,
    ) -> Vec<u8> {
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut uvs = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                positions.extend([x as f32, y as f32, 0.0]);
                normals.extend([0.0f32, 0.0, 1.0]);
                uvs.extend([x as f32 / 3.0, y as f32 / 3.0]);
            }
        }
        let mut indices: Vec<u16> = Vec::new();
        for y in 0..3u16 {
            for x in 0..3u16 {
                let a = y * 4 + x;
                indices.extend([a, a + 1, a + 4, a + 1, a + 5, a + 4]);
            }
        }
        let mut binary = Vec::new();
        for value in positions.iter().chain(&normals).chain(&uvs) {
            binary.extend(value.to_le_bytes());
        }
        for value in &indices {
            binary.extend(value.to_le_bytes());
        }
        let mut primitives = vec![json!({
            "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 },
            "indices": 3,
            "material": 0
        })];
        if broken {
            primitives.push(json!({ "attributes": { "POSITION": 0 } }));
        }
        let mut document = json!({
            "asset": { "version": "2.0" },
            "buffers": [{ "byteLength": binary.len() }],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 192 },
                { "buffer": 0, "byteOffset": 192, "byteLength": 192 },
                { "buffer": 0, "byteOffset": 384, "byteLength": 128 },
                { "buffer": 0, "byteOffset": 512, "byteLength": indices.len() * 2 }
            ],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": 16, "type": "VEC3",
                  "min": [0.0, 0.0, 0.0], "max": [3.0, 3.0, 0.0] },
                { "bufferView": 1, "componentType": 5126, "count": 16, "type": "VEC3" },
                { "bufferView": 2, "componentType": 5126, "count": 16, "type": "VEC2" },
                { "bufferView": 3, "componentType": 5123, "count": indices.len(), "type": "SCALAR" }
            ],
            "materials": [{
                "extras": { "openSkyrim": { "shaderFamily": "lighting", "lightingShaderType": "default" } }
            }],
            "meshes": [{ "primitives": primitives }],
            "nodes": [{ "mesh": 0 }],
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
        });
        if let Some(uri) = uri {
            document["images"] = json!([{ "uri": uri }]);
            document["textures"] = json!([{ "source": 0 }]);
            document["materials"][0]["pbrMetallicRoughness"] =
                json!({ "baseColorTexture": { "index": 0 } });
        }
        if blend {
            document["materials"][0]["alphaMode"] = json!("BLEND");
        }
        if billboard {
            document["nodes"][0]["extras"] =
                json!({ "openSkyrim": { "billboard": "rotateAboutUp" } });
        }
        glb(&document, &binary)
    }

    /// A complete indexed triangle, with its own three positions, normals and UVs.
    fn single_triangle_glb() -> Vec<u8> {
        let positions = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let normals = [0.0f32, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
        let uvs = [0.0f32, 0.0, 1.0, 0.0, 0.0, 1.0];
        let indices = [0u16, 1, 2];
        let mut binary = Vec::new();
        for value in positions.iter().chain(&normals).chain(&uvs) {
            binary.extend(value.to_le_bytes());
        }
        for value in indices {
            binary.extend(value.to_le_bytes());
        }
        glb(
            &json!({
                "asset": { "version": "2.0" },
                "buffers": [{ "byteLength": binary.len() }],
                "bufferViews": [
                    { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
                    { "buffer": 0, "byteOffset": 36, "byteLength": 36 },
                    { "buffer": 0, "byteOffset": 72, "byteLength": 24 },
                    { "buffer": 0, "byteOffset": 96, "byteLength": 6 }
                ],
                "accessors": [
                    { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                      "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] },
                    { "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3" },
                    { "bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2" },
                    { "bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR" }
                ],
                "materials": [{
                    "extras": { "openSkyrim": { "shaderFamily": "lighting", "lightingShaderType": "default" } }
                }],
                "meshes": [{ "primitives": [{
                    "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 },
                    "indices": 3, "material": 0
                }] }],
                "nodes": [{ "mesh": 0 }], "scene": 0,
                "scenes": [{ "nodes": [0] }]
            }),
            &binary,
        )
    }

    fn clear() -> Vec<u8> {
        serialize_subrecords(&[(b"EDID".to_vec(), b"invented\0".to_vec())])
    }

    fn add(manifest: &mut ConversionManifest, root: &Path, path: &str, bytes: &[u8]) {
        let output = root.join(path);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(output, bytes).unwrap();
        manifest.entries.insert(
            path.into(),
            CacheEntry {
                animation_diagnostics: None,
                source_hash: hash_bytes(bytes),
                output: path.into(),
                output_size: bytes.len() as u64,
                output_hash: hash_bytes(bytes),
            },
        );
    }

    /// Real writer/authority/finalizer, on invented inputs only. Texture bytes
    /// exercise the authority's hash contract; this fixture does not claim KTX decode.
    pub(crate) fn published() -> tempfile::TempDir {
        published_inputs(true)
    }

    /// Real producer fixture with loader-valid, textureless geometry.
    pub(crate) fn published_for_loader() -> tempfile::TempDir {
        published_inputs(false)
    }

    /// Real producer inputs for renderer all-distance selection, before certification.
    pub(crate) fn published_for_all_distance_selection(
        is_tree: bool,
        cell: IVec2,
    ) -> tempfile::TempDir {
        published_options_with_inputs(
            FixtureOptions {
                with_texture_and_rejected_primitive: false,
                inventory: false,
                triangle_threshold: 1,
                single_triangle: false,
                billboard: false,
                runtime_inventory: false,
                base_record_type: if is_tree { "TREE" } else { "STAT" },
            },
            |db| {
                let side = shared::cluster_recipe::classify::CELL_SIZE;
                db.execute(
                    "UPDATE cells SET grid_x=?1,grid_y=?2",
                    params![cell.x, cell.y],
                )
                .unwrap();
                db.execute(
                    "UPDATE \"references\" SET pos_x=pos_x+?1,pos_y=pos_y+?2,header_flags=?3",
                    params![
                        cell.x as f32 * side,
                        cell.y as f32 * side,
                        if is_tree { 0 } else { 1 << 15 }
                    ],
                )
                .unwrap();
                db.execute("UPDATE statics SET flags=?1", [1 << 15])
                    .unwrap();
            },
        )
    }

    /// Loader-valid geometry with two distinct references in the parent world
    /// and one in the child world, for readiness group aggregation tests.
    pub(crate) fn published_for_readiness() -> tempfile::TempDir {
        published_options_with_inputs(
            FixtureOptions {
                with_texture_and_rejected_primitive: false,
                inventory: false,
                triangle_threshold: 1,
                single_triangle: false,
                billboard: false,
                runtime_inventory: false,
                base_record_type: "STAT",
            },
            |db| {
                db.execute(
                    "INSERT INTO \"references\" (id,cell_id,worldspace_id,base_form_id,is_exterior,
                     pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,data)
                     SELECT ?1,cell_id,worldspace_id,base_form_id,is_exterior,
                     pos_x+1.0,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,data
                     FROM \"references\" WHERE id=?2",
                    params![0x10A_u32, PARENT_REF],
                )
                .unwrap();
            },
        )
    }

    /// Complete converter coverage whose actual loader template requires billboard motion.
    pub(crate) fn published_for_billboard_loader() -> tempfile::TempDir {
        published_options(FixtureOptions {
            with_texture_and_rejected_primitive: false,
            inventory: false,
            triangle_threshold: 1,
            single_triangle: false,
            billboard: true,
            runtime_inventory: false,
            base_record_type: "STAT",
        })
    }

    pub(crate) fn published_for_inventory() -> tempfile::TempDir {
        published_inputs_with_inventory(true, true)
    }

    /// Legacy package with below-threshold static primitives and no replacement records.
    pub(crate) fn published_for_legacy_threshold_inventory() -> tempfile::TempDir {
        published_options(FixtureOptions {
            with_texture_and_rejected_primitive: false,
            inventory: true,
            triangle_threshold: 1024,
            single_triangle: false,
            billboard: false,
            runtime_inventory: false,
            base_record_type: "STAT",
        })
    }

    /// Inventory plus initially-disabled STAT reference 0x109, in WORLD at (-2, 1),
    /// with the same model path and pose as the other refused inventory references.
    pub(crate) fn published_for_runtime_inventory() -> tempfile::TempDir {
        published_options(FixtureOptions {
            with_texture_and_rejected_primitive: true,
            inventory: true,
            triangle_threshold: 1,
            single_triangle: false,
            billboard: false,
            runtime_inventory: true,
            base_record_type: "STAT",
        })
    }

    /// Runtime-state refusals with a model-less parent outside the drawn reference scope.
    pub(crate) fn published_for_runtime_draw_states() -> tempfile::TempDir {
        published_options_with_inputs(
            FixtureOptions {
                with_texture_and_rejected_primitive: true,
                inventory: true,
                triangle_threshold: 1,
                single_triangle: false,
                billboard: false,
                runtime_inventory: true,
                base_record_type: "STAT",
            },
            |db| {
                // All children share the distant placement. The parent has no render model.
                for (reference, base, parent, opposite) in [
                    (0x110, 0x15, None, 0),
                    (0x111, 0x11, Some(0x110), 0),
                    (0x112, 0x11, Some(0x110), 1),
                    (0x113, 0x11, None, 0),
                ] {
                    db.execute(
                        "INSERT INTO \"references\" (id,cell_id,worldspace_id,base_form_id,is_exterior,
                         pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,data,
                         enable_parent_id,enable_parent_flags)
                         SELECT ?1,cell_id,worldspace_id,?2,is_exterior,pos_x,pos_y,pos_z,
                         rot_x,rot_y,rot_z,scale,0,data,?3,?4 FROM \"references\" WHERE id=?5",
                        params![reference, base, parent, opposite, RUNTIME_REF],
                    ).unwrap();
                }
                db.execute("INSERT INTO scripts(form_id,script_name,vmad,properties_json) VALUES(0x113,'InventedStateScript',X'','{}')", []).unwrap();
            },
        )
    }

    fn published_inputs(with_texture_and_rejected_primitive: bool) -> tempfile::TempDir {
        published_inputs_with_inventory(with_texture_and_rejected_primitive, false)
    }

    fn published_inputs_with_inventory(
        with_texture_and_rejected_primitive: bool,
        inventory: bool,
    ) -> tempfile::TempDir {
        published_options(FixtureOptions {
            with_texture_and_rejected_primitive,
            inventory,
            triangle_threshold: 1,
            single_triangle: false,
            billboard: false,
            runtime_inventory: false,
            base_record_type: "STAT",
        })
    }

    struct FixtureOptions {
        with_texture_and_rejected_primitive: bool,
        inventory: bool,
        triangle_threshold: u32,
        single_triangle: bool,
        billboard: bool,
        runtime_inventory: bool,
        base_record_type: &'static str,
    }

    fn published_options(options: FixtureOptions) -> tempfile::TempDir {
        published_options_with_inputs(options, |_| {})
    }

    fn published_options_with_inputs(
        options: FixtureOptions,
        edit: impl FnOnce(&Connection),
    ) -> tempfile::TempDir {
        published_options_with_model(options, MODEL, r"Arch\Rock.nif", edit)
    }

    pub(crate) fn published_for_marker_policy(
        flagged: bool,
        refuse_geometry: bool,
    ) -> tempfile::TempDir {
        published_options_with_model(
            FixtureOptions {
                with_texture_and_rejected_primitive: false,
                inventory: false,
                triangle_threshold: if refuse_geometry { u32::MAX } else { 1 },
                single_triangle: false,
                billboard: false,
                runtime_inventory: false,
                base_record_type: "STAT",
            },
            if flagged {
                MODEL
            } else {
                "meshes/arch/rockmarker.glb"
            },
            if flagged {
                r"Arch\Rock.nif"
            } else {
                r"Arch\RockMarker.nif"
            },
            |db| {
                db.execute(
                    "UPDATE statics SET flags=?1 WHERE id=0x11",
                    [if flagged { 0x00800000 } else { 0 }],
                )
                .unwrap();
            },
        )
    }

    fn published_options_with_model(
        options: FixtureOptions,
        model_path: &str,
        source_model_path: &str,
        edit: impl FnOnce(&Connection),
    ) -> tempfile::TempDir {
        let FixtureOptions {
            with_texture_and_rejected_primitive,
            inventory,
            triangle_threshold,
            single_triangle,
            billboard,
            runtime_inventory,
            base_record_type,
        } = options;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(
            root.join("OWNER.txt"),
            "engine cluster reader producer fixture; tempfile-owned synthetic inputs\n",
        )
        .unwrap();
        let lock = AssetLock::acquire_exclusive(root).unwrap();
        let mut manifest = ConversionManifest::default();
        if with_texture_and_rejected_primitive {
            add(&mut manifest, root, TEXTURE, b"invented texture bytes");
        }
        let model = if single_triangle {
            assert!(!with_texture_and_rejected_primitive);
            assert!(!billboard);
            single_triangle_glb()
        } else {
            grid_glb_with_billboard(
                with_texture_and_rejected_primitive,
                with_texture_and_rejected_primitive.then_some("../../textures/rock.ktx2"),
                false,
                billboard,
            )
        };
        add(&mut manifest, root, model_path, &model);
        if inventory {
            add(
                &mut manifest,
                root,
                BLEND_MODEL,
                &grid_glb(false, None, true),
            );
        }
        let db = Connection::open(root.join("skyrim_world.db")).unwrap();
        create_tables(&db).unwrap();
        db.execute("INSERT INTO lod_build VALUES (1, ?1)", ["cd".repeat(32)])
            .unwrap();
        db.execute(
            "INSERT INTO worldspaces(id,editor_id,parent_world,flags,lod_origin_x,lod_origin_y) VALUES (?1,'Parent',NULL,0,-32,-32)",
            [WORLD],
        )
        .unwrap();
        db.execute(
            "INSERT INTO worldspaces(id,editor_id,parent_world,flags,lod_origin_x,lod_origin_y) VALUES (?1,'Child',?2,0,NULL,NULL)",
            params![CHILD, WORLD],
        )
        .unwrap();
        for (cell, world) in [(PARENT_CELL, WORLD), (CHILD_CELL, CHILD)] {
            db.execute(
                "INSERT INTO cells(id,worldspace_id,grid_x,grid_y,flags) VALUES(?1,?2,0,0,0)",
                params![cell, world],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO records(form_id,record_type,load_order,data) VALUES(0x11,?1,0,?2)",
            params![base_record_type, clear()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO statics(id,editor_id,model_path,flags) VALUES(0x11,'Invented',?1,0)",
            [source_model_path],
        )
        .unwrap();
        for (reference, cell, world, x) in [
            (PARENT_REF, PARENT_CELL, WORLD, 100.0),
            (CHILD_REF, CHILD_CELL, CHILD, 200.0),
        ] {
            db.execute("INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,data)
                VALUES(?1,?2,?3,0x11,1,?4,100.0,10.0,0.0,0.0,0.0,1.0,0,?5)",params![reference,cell,world,x,clear()]).unwrap();
        }
        if inventory {
            db.execute(
                "INSERT INTO cells(id,interior_name,flags) VALUES(?1,'InventedInterior',1)",
                [INTERIOR_CELL],
            )
            .unwrap();
            for (base, record_type, path, data) in [
                (0x12, "STAT", Some("Arch\\Blend.nif"), clear()),
                (0x13, "STAT", Some("Arch\\Missing.nif"), clear()),
                (0x14, "TREE", Some("Arch\\Rock.nif"), clear()),
                (0x15, "STAT", None, clear()),
                (0x16, "STAT", Some("Arch\\Rock.nif"), vec![0xff]),
            ] {
                db.execute(
                    "INSERT INTO records(form_id,record_type,load_order,data) VALUES(?1,?2,0,?3)",
                    params![base, record_type, data],
                )
                .unwrap();
                db.execute(
                    "INSERT INTO statics(id,editor_id,model_path,flags) VALUES(?1,'InventedInventory',?2,0)",
                    params![base, path],
                )
                .unwrap();
            }
            let pose = inventory_pose();
            for (reference, base, cell, world, exterior, data) in [
                (BLEND_REF, 0x12, CHILD_CELL, Some(CHILD), 1, clear()),
                (
                    GUARDED_REF,
                    0x11,
                    PARENT_CELL,
                    Some(WORLD),
                    1,
                    serialize_subrecords(&[(b"XALP".to_vec(), 0.5f32.to_le_bytes().to_vec())]),
                ),
                (MISSING_MODEL_REF, 0x13, CHILD_CELL, Some(CHILD), 1, clear()),
                (UNPARSED_REF, 0x16, PARENT_CELL, Some(WORLD), 1, clear()),
                (TREE_REF, 0x14, CHILD_CELL, Some(CHILD), 1, clear()),
                (INTERIOR_REF, 0x11, INTERIOR_CELL, None, 0, clear()),
                (NO_MODEL_REF, 0x15, PARENT_CELL, Some(WORLD), 1, clear()),
            ] {
                db.execute(
                    "INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,
                         pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,data)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,?13)",
                    params![
                        reference,
                        cell,
                        world,
                        base,
                        exterior,
                        pose.position[0],
                        pose.position[1],
                        pose.position[2],
                        pose.rotation[0],
                        pose.rotation[1],
                        pose.rotation[2],
                        pose.scale,
                        data
                    ],
                )
                .unwrap();
            }
        }
        if runtime_inventory {
            let pose = inventory_pose();
            db.execute(
                "INSERT INTO \"references\"(id,cell_id,worldspace_id,base_form_id,is_exterior,
                     pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale,header_flags,data)
                 VALUES(?1,?2,?3,0x11,1,?4,?5,?6,?7,?8,?9,?10,2048,?11)",
                params![
                    RUNTIME_REF,
                    PARENT_CELL,
                    WORLD,
                    pose.position[0],
                    pose.position[1],
                    pose.position[2],
                    pose.rotation[0],
                    pose.rotation[1],
                    pose.rotation[2],
                    pose.scale,
                    clear()
                ],
            )
            .unwrap();
        }
        edit(&db);
        drop(db);
        let settings = StageSettings {
            mode: LodMode::Cluster,
            triangle_threshold,
            jobs: 1,
            assets_root: None,
            read_database: None,
            area: None,
            child_worldspaces: vec![CHILD],
            include_trees: base_record_type == "TREE",
            map_references: true,
        };
        let probe = ProbeResult {
            passed: true,
            missing: Vec::new(),
            backend: "test".into(),
            adapter_name: "invented reader fixture, no GPU".into(),
            json: json!({"test":true}),
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
        manifest.complete = true;
        manifest
            .save(&root.join("conversion-manifest.json"))
            .unwrap();
        drop(authority);
        drop(lock);
        // The converter wrote one folder; the engine reads the base + overlay layout.
        crate::cluster::overlay_fixture::split_full_copy(root);
        directory
    }

    fn triangle_key(points: [[f32; 3]; 3]) -> [[u32; 3]; 3] {
        let points = points.map(|point| point.map(f32::to_bits));
        // Cluster construction may rotate a triangle's corners; preserve its winding.
        let first = (0..3).min_by_key(|&index| points[index]).unwrap();
        [
            points[first],
            points[(first + 1) % 3],
            points[(first + 2) % 3],
        ]
    }

    fn assert_source_triangles_in_leaf_payload(
        root: &Path,
        db: &Connection,
        package: &CertifiedPackage,
        expected_triangles: usize,
    ) {
        let source_bytes = fs::read(root.join(MODEL)).unwrap();
        let source = converter::cluster::glb_input::load_model(&source_bytes, MODEL).unwrap();
        assert_eq!(source.len(), 1);
        assert!(source[0].reject.is_none());
        let geometry = &source[0].geometry;
        assert_eq!(geometry.triangle_count(), expected_triangles);
        if expected_triangles == 1 {
            assert_eq!(geometry.positions.len(), 3);
            assert_eq!(geometry.normals.len(), 3);
            assert_eq!(geometry.uvs.len(), 3);
            assert_eq!(geometry.indices, [0, 1, 2]);
        }
        let mut expected: Vec<_> = geometry
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|triangle| {
                triangle_key(std::array::from_fn(|corner| {
                    geometry.positions[triangle[corner] as usize]
                }))
            })
            .collect();
        expected.sort_unstable();
        let (path, source_triangles): (String, usize) = db
            .query_row(
                "SELECT payload_path,source_triangles FROM cluster_meshes
                 WHERE model_path=?1 AND primitive_index=0",
                [MODEL],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(source_triangles, expected_triangles);
        let bytes =
            fs::read(crate::cluster::overlay_fixture::overlay_path(root).join(path)).unwrap();
        let mesh = read_mcl(&bytes).unwrap();
        assert_eq!(
            hex(&mesh.builder_identity),
            package.envelope().builder_identity
        );
        assert_eq!(hex(&mesh.mesh_hash), hash_bytes(&source_bytes));
        assert_eq!(mesh.primitive_index, 0);
        let mut decoded = Vec::new();
        let mut leaf_triangles = 0;
        for index in 0..mesh.cluster_count as usize {
            let cluster = mesh.cluster(index);
            if cluster.refined_group != NO_GROUP {
                continue;
            }
            leaf_triangles += usize::from(cluster.triangle_count);
            for triangle in mesh.cluster_triangles(&cluster).as_chunks::<3>().0 {
                let points = std::array::from_fn(|corner| {
                    let local = triangle[corner];
                    assert!(local < cluster.vertex_count);
                    let vertex =
                        mesh.cluster_vertex(cluster.vertex_offset as usize + usize::from(local));
                    assert!(vertex < mesh.vertex_count);
                    let point = mesh.position(vertex as usize);
                    assert!(point.into_iter().all(f32::is_finite));
                    point
                });
                decoded.push(triangle_key(points));
            }
        }
        assert_eq!(leaf_triangles, expected_triangles);
        decoded.sort_unstable();
        assert_eq!(
            decoded, expected,
            "leaf geometry must retain every source triangle exactly once"
        );
    }

    fn assert_threshold_transition(single_triangle: bool, expected_triangles: usize) {
        let fixture = |triangle_threshold| {
            published_options(FixtureOptions {
                with_texture_and_rejected_primitive: false,
                inventory: true,
                triangle_threshold,
                single_triangle,
                billboard: false,
                runtime_inventory: false,
                base_record_type: "STAT",
            })
        };
        let high = fixture(1024);
        let low = fixture(1);
        let high_package =
            crate::cluster::overlay_fixture::open_fixture(high.path(), false).unwrap();
        let low_package = crate::cluster::overlay_fixture::open_fixture(low.path(), false).unwrap();
        assert_eq!(
            fs::read(high.path().join(MODEL)).unwrap(),
            fs::read(low.path().join(MODEL)).unwrap()
        );
        assert_eq!(
            high_package.envelope().core_identity,
            low_package.envelope().core_identity
        );
        assert_eq!(high_package.envelope().core_identity, "cd".repeat(32));
        assert_ne!(
            high_package.envelope().builder_identity,
            low_package.envelope().builder_identity
        );
        assert_eq!(high_package.records().count(), 0);
        assert_eq!(low_package.records().count(), 2);
        for (directory, package, threshold) in
            [(&high, &high_package, 1024), (&low, &low_package, 1)]
        {
            let db = crate::cluster::overlay_fixture::read_db(directory.path());
            assert_eq!(
                db.query_row(
                    "SELECT triangle_threshold FROM cluster_build WHERE id=1",
                    [],
                    |row| row.get::<_, u32>(0)
                )
                .unwrap(),
                threshold
            );
            for reference in [PARENT_REF, CHILD_REF] {
                let placed = package.placed_reference(reference).unwrap();
                assert_eq!(placed.snapshot.eligibility, Eligibility::Eligible);
                assert_eq!(
                    placed.snapshot.model,
                    Some(hash_bytes(&fs::read(directory.path().join(MODEL)).unwrap()))
                );
                let primitive: (u32, String, String) = db
                    .query_row(
                        "SELECT primitive_index,status,reason FROM cluster_reference_coverage
                     WHERE reference_id=?1 AND scope='primitive'",
                        [reference],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
                if threshold == 1024 {
                    assert_eq!(placed.coverage, CoverageState::Partial);
                    assert_eq!(placed.stock_primitives, Some(BTreeSet::from([0])));
                    assert_eq!(
                        primitive,
                        (0, "rejected".into(), "below_triangle_threshold".into())
                    );
                    assert!(package.reference(reference).is_none());
                    assert!(!package.reference_complete(reference));
                    assert!(package.unclustered_primitives(reference).is_none());
                } else {
                    assert_eq!(placed.coverage, CoverageState::Complete);
                    assert_eq!(placed.stock_primitives, Some(BTreeSet::new()));
                    assert_eq!(primitive, (0, "clustered".into(), String::new()));
                    assert!(package.reference_complete(reference));
                    assert_eq!(
                        package.unclustered_primitives(reference),
                        Some(&BTreeSet::new())
                    );
                }
            }
            let blend = package.placed_reference(BLEND_REF).unwrap();
            assert_eq!(blend.coverage, CoverageState::Partial);
            assert_eq!(blend.stock_primitives, Some(BTreeSet::from([0])));
            let reason: String = db.query_row(
                "SELECT reason FROM cluster_reference_coverage WHERE reference_id=?1 AND scope='primitive'",
                [BLEND_REF], |row| row.get(0),
            ).unwrap();
            assert_eq!(reason, "not_opaque");
            for (reference, coverage, eligibility) in [
                (
                    GUARDED_REF,
                    CoverageState::NotEligible,
                    Eligibility::NotEligible("unresolved_reference_override:XALP".into()),
                ),
                (
                    MISSING_MODEL_REF,
                    CoverageState::Unknown,
                    Eligibility::Unknown("model_not_converted".into()),
                ),
            ] {
                let placed = package.placed_reference(reference).unwrap();
                assert_eq!(placed.coverage, coverage);
                assert_eq!(placed.snapshot.eligibility, eligibility);
                assert!(placed.stock_primitives.is_none());
                assert!(package.reference(reference).is_none());
                assert!(!package.reference_complete(reference));
            }
            if threshold == 1 {
                let mut mapped = BTreeSet::new();
                for (key, identity) in package.records() {
                    assert!(mapped.insert(identity.reference));
                    assert_eq!(identity.primitive, 0);
                    let path: String = db.query_row(
                        "SELECT payload_path FROM cluster_chunks WHERE worldspace_id=?1 AND tier=?2 AND anchor_x=?3 AND anchor_y=?4",
                        params![key.worldspace,key.tier,key.anchor[0],key.anchor[1]], |row| row.get(0),
                    ).unwrap();
                    let bytes = fs::read(
                        crate::cluster::overlay_fixture::overlay_path(directory.path()).join(path),
                    )
                    .unwrap();
                    let block = read_mci(&bytes).unwrap();
                    let record = block.instance(key.ordinal as usize);
                    let mesh = block.mesh(record.mesh as usize);
                    assert_eq!(hex(&mesh.mesh_hash), identity.model_hash);
                    assert_eq!(mesh.primitive_index, identity.primitive);
                }
                assert_eq!(mapped, BTreeSet::from([PARENT_REF, CHILD_REF]));
                assert_source_triangles_in_leaf_payload(
                    directory.path(),
                    &db,
                    package,
                    expected_triangles,
                );
            }
        }
    }

    #[test]
    fn threshold_one_clusters_the_real_eighteen_triangle_source_without_relaxing_refusals() {
        assert_threshold_transition(false, 18);
    }

    #[test]
    fn threshold_one_clusters_a_complete_indexed_single_triangle_without_losing_leaf_geometry() {
        assert_threshold_transition(true, 1);
    }

    #[test]
    fn cluster_sources_certify_without_baked_tables_and_reject_missing_payloads() {
        let directory = published_for_loader();
        let root = directory.path();
        let db = crate::cluster::overlay_fixture::base_db(root);
        // lod_build retains the shared core identity; it is not a baked geometry product.
        // Core export always creates these first three tables; native inventory is optional.
        let baked_tables = [
            ("lod_chunks_spatial", false),
            ("lod_chunks", false),
            ("lod", false),
            ("lod_grid", true),
            ("lod_block", true),
            ("lod_tree_instance", true),
            ("lod_tree_type", true),
        ];
        for (table, optional) in baked_tables {
            let if_exists = if optional { "IF EXISTS " } else { "" };
            db.execute_batch(&format!("DROP TABLE {if_exists}{table};"))
                .unwrap();
            let present: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!present, "baked geometry table {table} must be absent");
        }
        let core_identity = shared::extensions::core_build_identity(&db)
            .unwrap()
            .expect("fixture must retain lod_build core identity");
        drop(db);
        let package = crate::cluster::overlay_fixture::open_fixture(root, false).unwrap();
        assert_eq!(package.envelope().core_identity, core_identity);
        assert_eq!(package.records().count(), 2);
        let overlay = crate::cluster::overlay_fixture::overlay_path(root);
        let payload: String = crate::cluster::overlay_fixture::overlay_db(root)
            .query_row(
                "SELECT payload_path FROM cluster_chunks LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(package);
        fs::remove_file(overlay.join(&payload)).unwrap();
        assert!(crate::cluster::overlay_fixture::open_fixture(root, false).is_err());
    }

    #[test]
    fn real_producer_records_retain_child_placement_and_partial_primitive_identity() {
        let directory = published();
        let root = directory.path();
        let package = crate::cluster::overlay_fixture::open_fixture(root, false).unwrap();
        assert_eq!(package.records().count(), 2);
        let db = crate::cluster::overlay_fixture::read_db(root);
        let path: String = db
            .query_row("SELECT payload_path FROM cluster_chunks", [], |row| {
                row.get(0)
            })
            .unwrap();
        let bytes =
            fs::read(crate::cluster::overlay_fixture::overlay_path(root).join(path)).unwrap();
        let block = read_mci(&bytes).unwrap();
        assert_eq!(
            (
                block.worldspace_id,
                block.tier,
                block.anchor_x,
                block.anchor_y
            ),
            (WORLD, 16, 2, 2)
        );
        assert_eq!(block.instance_count, 2);
        let model_hash = hash_bytes(&fs::read(root.join(MODEL)).unwrap());
        assert_eq!(package.reference_model_path(PARENT_REF), Some(MODEL));
        assert_eq!(package.reference_model_path(CHILD_REF), Some(MODEL));
        assert_eq!(package.reference_model_path(0x999), None);
        let mut references = BTreeSet::new();
        for ordinal in 0..block.instance_count {
            let key = RecordKey {
                worldspace: WORLD,
                tier: 16,
                anchor: [2, 2],
                ordinal,
            };
            let identity = package
                .record(&key)
                .expect("every actual record must be indexed");
            let (placed, cell) = match identity.reference {
                PARENT_REF => (WORLD, PARENT_CELL),
                CHILD_REF => (CHILD, CHILD_CELL),
                other => panic!("unexpected reference {other:#x}"),
            };
            assert!(references.insert(identity.reference));
            assert_eq!((identity.worldspace, identity.source_cell), (placed, cell));
            assert_eq!((&identity.model_hash, identity.primitive), (&model_hash, 0));
            let record = block.instance(ordinal as usize);
            assert_eq!(
                record.source_worldspace_id,
                if placed == WORLD { 0 } else { CHILD }
            );
            let mesh = block.mesh(record.mesh as usize);
            assert_eq!(
                (hex(&mesh.mesh_hash), mesh.primitive_index),
                (model_hash.clone(), 0)
            );
            let snapshot = package.reference(identity.reference).unwrap();
            let inventory = package.placed_reference(identity.reference).unwrap();
            assert_eq!(&inventory.snapshot, snapshot);
            assert_eq!(inventory.coverage, CoverageState::Partial);
            assert_eq!(inventory.stock_primitives, Some(BTreeSet::from([1])));
            assert_eq!(inventory.model_path.as_deref(), Some(MODEL));
            assert_eq!(
                Some(&inventory.pose),
                package.reference_pose(identity.reference)
            );
            assert_eq!(
                (snapshot.placed_worldspace_id, snapshot.source_cell_id),
                (placed, cell)
            );
            assert_eq!(
                snapshot.transform.unwrap().map(f32::to_bits),
                record.transform.map(f32::to_bits)
            );
            assert_eq!(
                coverage_state(&db, identity.reference).unwrap(),
                CoverageState::Partial
            );
            assert!(!package.reference_complete(identity.reference));
            assert_eq!(
                package.unclustered_primitives(identity.reference),
                Some(&BTreeSet::from([1]))
            );
        }
        assert_eq!(references, BTreeSet::from([PARENT_REF, CHILD_REF]));
        // Rejected primitive 1 is in source coverage, never in the drawable record index.
        assert!(
            package
                .records()
                .all(|(_, identity)| identity.primitive == 0)
        );
    }

    #[test]
    fn a_real_fully_clustered_reference_is_complete_but_an_unknown_reference_is_not() {
        let directory = published_for_loader();
        let package =
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
        for reference in [PARENT_REF, CHILD_REF] {
            assert!(package.reference_complete(reference));
            let placed = package.placed_reference(reference).unwrap();
            assert_eq!(placed.coverage, CoverageState::Complete);
            assert_eq!(placed.stock_primitives, Some(BTreeSet::new()));
            assert_eq!(
                package.unclustered_primitives(reference),
                Some(&BTreeSet::new())
            );
        }
        assert!(!package.reference_complete(0x999));
        assert!(package.unclustered_primitives(0x999).is_none());
    }

    #[test]
    fn real_producer_inventory_retains_wholly_rejected_child_placement_without_mapped_membership() {
        let directory = published_inputs_with_inventory(true, true);
        let root = directory.path();
        let package = crate::cluster::overlay_fixture::open_fixture(root, false).unwrap();
        let placed = package.placed_reference(BLEND_REF).unwrap();
        assert_eq!(placed.coverage, CoverageState::Partial);
        assert_eq!(placed.stock_primitives, Some(BTreeSet::from([0])));
        assert_eq!(placed.snapshot.eligibility, Eligibility::Eligible);
        assert_eq!(
            placed.snapshot.model,
            Some(hash_bytes(&fs::read(root.join(BLEND_MODEL)).unwrap()))
        );
        assert_eq!(placed.model_path.as_deref(), Some(BLEND_MODEL));
        assert_eq!(placed.pose, inventory_pose());
        assert_eq!(placed.snapshot.placed_worldspace_id, CHILD);
        assert_eq!(placed.snapshot.source_cell_id, CHILD_CELL);
        assert_eq!(placed.snapshot.cell, (INVENTORY_GRID.x, INVENTORY_GRID.y));
        assert_eq!(placed.snapshot.block.0, WORLD);
        let db = crate::cluster::overlay_fixture::read_db(root);
        let owning_grid: (i32, i32) = db
            .query_row(
                "SELECT grid_x,grid_y FROM cells WHERE id=?1",
                [CHILD_CELL],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_ne!(placed.snapshot.cell, owning_grid);
        let rejected: (u32, String) = db
            .query_row(
                "SELECT primitive_index,reason FROM cluster_reference_coverage
                 WHERE reference_id=?1 AND scope='primitive' AND status='rejected'",
                [BLEND_REF],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(rejected, (0, "not_opaque".into()));
        assert!(
            package
                .placed_references_in_cell(CHILD, INVENTORY_GRID)
                .any(|(id, entry)| id == BLEND_REF && entry == placed)
        );
        for (world, grid) in [(WORLD, INVENTORY_GRID), (CHILD, IVec2::ZERO)] {
            assert!(
                !package
                    .placed_references_in_cell(world, grid)
                    .any(|(id, _)| id == BLEND_REF)
            );
        }
        // Adding a refused primitive to inventory creates no mapped replacement candidate.
        assert_eq!(package.records().count(), 2);
        for reference in [
            BLEND_REF,
            GUARDED_REF,
            MISSING_MODEL_REF,
            UNPARSED_REF,
            NO_MODEL_REF,
        ] {
            assert!(package.reference(reference).is_none());
            assert!(package.reference_model_path(reference).is_none());
            assert!(package.reference_pose(reference).is_none());
            assert!(!package.reference_complete(reference));
            assert!(package.unclustered_primitives(reference).is_none());
            assert!(
                !package
                    .records()
                    .any(|(_, record)| record.reference == reference)
            );
        }
        assert_eq!(
            package
                .records_in_placed_cell(CHILD, INVENTORY_GRID)
                .count(),
            0
        );
    }

    #[test]
    fn real_producer_inventory_retains_recorded_look_and_runtime_refusal_classes() {
        let directory = published_for_runtime_inventory();
        let root = directory.path();
        let package = crate::cluster::overlay_fixture::open_fixture(root, false).unwrap();
        let db = crate::cluster::overlay_fixture::read_db(root);
        for (reference, scope, reason, expected) in [
            (
                PARENT_REF,
                "primitive",
                "not_indexed_triangles",
                StockRefusalClass::Other,
            ),
            (
                CHILD_REF,
                "primitive",
                "not_indexed_triangles",
                StockRefusalClass::Other,
            ),
            (
                BLEND_REF,
                "primitive",
                "not_opaque",
                StockRefusalClass::Look,
            ),
            (
                GUARDED_REF,
                "reference",
                "unresolved_reference_override:XALP",
                StockRefusalClass::Look,
            ),
            (
                MISSING_MODEL_REF,
                "reference",
                "model_not_converted",
                StockRefusalClass::Other,
            ),
            (
                UNPARSED_REF,
                "reference",
                "unparsed_base_record",
                StockRefusalClass::Other,
            ),
            (
                NO_MODEL_REF,
                "reference",
                "no_model",
                StockRefusalClass::Other,
            ),
            (
                RUNTIME_REF,
                "reference",
                "initially_disabled",
                StockRefusalClass::Runtime,
            ),
        ] {
            let recorded: String = db
                .query_row(
                    "SELECT reason FROM cluster_reference_coverage
                 WHERE reference_id=?1 AND scope=?2 AND status <> 'clustered'",
                    params![reference, scope],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(recorded, reason);
            let placed = package.placed_reference(reference).unwrap();
            assert_eq!(placed.refusal_class, expected, "{reference:#x}");
            assert!(placed.is_static_scenery);
        }
        for reference in [PARENT_REF, CHILD_REF, BLEND_REF] {
            assert_eq!(
                package.placed_reference(reference).unwrap().coverage,
                CoverageState::Partial
            );
        }
        let runtime = package.placed_reference(RUNTIME_REF).unwrap();
        assert_eq!(runtime.coverage, CoverageState::NotEligible);
        assert_eq!(
            runtime.snapshot.eligibility,
            Eligibility::NotEligible("initially_disabled".into())
        );
        assert_eq!(runtime.snapshot.placed_worldspace_id, WORLD);
        assert_eq!(runtime.snapshot.cell, (INVENTORY_GRID.x, INVENTORY_GRID.y));
        assert_eq!(runtime.model_path.as_deref(), Some(MODEL));
        assert_eq!(runtime.pose, inventory_pose());
        assert!(package.reference(RUNTIME_REF).is_none());
    }

    #[test]
    fn real_producer_marker_policy_controls_admission_and_retained_inventory() {
        for flagged in [false, true] {
            for refuse_geometry in [false, true] {
                let directory = published_for_marker_policy(flagged, refuse_geometry);
                let package =
                    crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
                for reference in [PARENT_REF, CHILD_REF] {
                    let placed = package.placed_reference(reference).unwrap();
                    if flagged {
                        assert_eq!(
                            placed.snapshot.eligibility,
                            Eligibility::NotEligible("base_is_marker".into())
                        );
                        assert_eq!(placed.coverage, CoverageState::NotEligible);
                        assert_eq!(placed.model_path, None);
                        assert!(package.reference(reference).is_none());
                    } else {
                        assert_eq!(placed.snapshot.eligibility, Eligibility::Eligible);
                        assert_eq!(
                            placed.model_path.as_deref(),
                            Some("meshes/arch/rockmarker.glb")
                        );
                        assert_eq!(
                            placed.coverage,
                            if refuse_geometry {
                                CoverageState::Partial
                            } else {
                                CoverageState::Complete
                            }
                        );
                        assert_eq!(package.reference(reference).is_some(), !refuse_geometry);
                    }
                    assert_eq!(
                        package
                            .records()
                            .any(|(_, record)| record.reference == reference),
                        !flagged && !refuse_geometry
                    );
                }
            }
        }
    }

    #[test]
    fn changing_only_authoritative_marker_flags_invalidates_admitted_geometry() {
        let directory = published_for_loader();
        let package =
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
        assert!(package.reference(PARENT_REF).is_some());
        drop(package);
        let lock = AssetLock::acquire_exclusive(directory.path()).unwrap();
        let db = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        db.execute(
            "UPDATE statics SET flags = flags | 0x00800000 WHERE id=0x11",
            [],
        )
        .unwrap();
        drop(db);
        drop(lock);
        let precheck = envelope::open(
            directory.path(),
            &crate::cluster::overlay_fixture::overlay_path(directory.path()),
            false,
        )
        .unwrap();
        drop(precheck);
        let error = match crate::cluster::overlay_fixture::open_fixture(directory.path(), false) {
            Ok(_) => panic!("authority admitted a newly flagged marker"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("DigestNotAuthoritative") || error.contains("EligibilityMismatch"),
            "must reach source authority: {error}"
        );
    }

    #[test]
    fn real_producer_inventory_uses_authoritative_base_type_for_static_scenery() {
        for base_record_type in ["STAT", "TREE", "MSTT", "ACTI"] {
            let directory = published_options(FixtureOptions {
                with_texture_and_rejected_primitive: false,
                inventory: false,
                triangle_threshold: 1,
                single_triangle: false,
                billboard: false,
                runtime_inventory: false,
                base_record_type,
            });
            let package =
                crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
            for reference in [PARENT_REF, CHILD_REF] {
                let placed = package.placed_reference(reference).unwrap();
                let is_static = matches!(base_record_type, "STAT" | "TREE");
                assert_eq!(placed.is_static_scenery, is_static, "{base_record_type}");
                assert_eq!(placed.is_tree, base_record_type == "TREE");
                if is_static {
                    assert_eq!(placed.refusal_class, StockRefusalClass::None);
                    assert_eq!(placed.coverage, CoverageState::Complete);
                } else {
                    assert_eq!(placed.refusal_class, StockRefusalClass::Runtime);
                    assert_eq!(
                        placed.snapshot.eligibility,
                        Eligibility::NotEligible("state_change_writer_unavailable".into())
                    );
                }
            }
        }
    }

    #[test]
    fn real_producer_inventory_distinguishes_refused_unknown_and_out_of_scope_inputs() {
        let directory = published_inputs_with_inventory(true, true);
        let root = directory.path();
        let package = crate::cluster::overlay_fixture::open_fixture(root, false).unwrap();
        let db = crate::cluster::overlay_fixture::read_db(root);
        for (reference, coverage, eligibility, path) in [
            (
                GUARDED_REF,
                CoverageState::NotEligible,
                Eligibility::NotEligible("unresolved_reference_override:XALP".into()),
                Some(MODEL),
            ),
            (
                MISSING_MODEL_REF,
                CoverageState::Unknown,
                Eligibility::Unknown("model_not_converted".into()),
                Some(MISSING_MODEL),
            ),
            (
                UNPARSED_REF,
                CoverageState::Unknown,
                Eligibility::Unknown("unparsed_base_record".into()),
                Some(MODEL),
            ),
            (
                NO_MODEL_REF,
                CoverageState::NotEligible,
                Eligibility::NotEligible("no_model".into()),
                None,
            ),
        ] {
            let placed = package.placed_reference(reference).unwrap();
            assert_eq!(placed.coverage, coverage);
            assert_eq!(coverage_state(&db, reference).unwrap(), coverage);
            assert_eq!(placed.snapshot.eligibility, eligibility);
            assert!(placed.snapshot.model.is_none());
            assert!(placed.stock_primitives.is_none());
            assert_eq!(placed.model_path.as_deref(), path);
            assert_eq!(placed.pose, inventory_pose());
        }
        for reference in [TREE_REF, INTERIOR_REF, 0x999] {
            assert_eq!(
                coverage_state(&db, reference).unwrap(),
                CoverageState::Absent
            );
            assert!(package.placed_reference(reference).is_none());
            assert!(
                !package
                    .placed_reference_cells
                    .values()
                    .flatten()
                    .any(|id| *id == reference)
            );
            assert!(package.reference(reference).is_none());
            assert!(!package.reference_complete(reference));
        }
    }

    #[test]
    fn real_producer_inventory_exactly_indexes_authoritative_scope_by_placed_world_and_cell() {
        let directory = published_inputs_with_inventory(true, true);
        let root = directory.path();
        let package = crate::cluster::overlay_fixture::open_fixture(root, false).unwrap();
        let db = crate::cluster::overlay_fixture::read_db(root);
        let declared: String = db
            .query_row(
                "SELECT classifier_scope FROM cluster_provenance WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let scope = Scope::from_declared_json(&declared).unwrap();
        let sources =
            shared::cluster_recipe::classify::read_sources(&db, &scope.child_worldspaces).unwrap();
        let coverage = read_coverage_states(&db).unwrap();
        for (&id, placed) in &package.placed_references {
            let legacy = shared::cluster_recipe::classify::scoped_reference_by_id(
                &db,
                &sources,
                id,
                scope.include_trees,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                placed.pose,
                ReferencePose {
                    position: legacy.position,
                    rotation: legacy.rotation,
                    scale: legacy.scale,
                }
            );
            assert_eq!(
                placed.model_path,
                spawned_model_path(legacy.model_path.as_deref(), legacy.base_is_marker)
            );
            assert_eq!(placed.is_tree, legacy.is_tree);
            assert_eq!(placed.base_has_distant_lod, legacy.base_has_distant_lod);
            assert_eq!(Some(&placed.coverage), coverage.get(&id));
            assert_eq!(placed.coverage, coverage_state(&db, id).unwrap());
        }
        drop(db);
        let authority = shared::cluster_recipe::authority::CoreAuthority::open_declared(
            Connection::open_with_flags(
                root.join("skyrim_world.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap(),
            root,
            published_files(&fs::read(root.join("conversion-manifest.json")).unwrap()).unwrap(),
            scope,
            package.envelope().builder_identity.clone(),
        )
        .unwrap();
        let expected = BTreeSet::from([
            PARENT_REF,
            CHILD_REF,
            BLEND_REF,
            GUARDED_REF,
            MISSING_MODEL_REF,
            UNPARSED_REF,
            NO_MODEL_REF,
        ]);
        assert_eq!(authority.examined_references(), expected);
        assert_eq!(
            package
                .placed_references
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            expected
        );
        let mut seen = BTreeSet::new();
        for (world, grid, expected_ids) in [
            (WORLD, IVec2::ZERO, BTreeSet::from([PARENT_REF])),
            (CHILD, IVec2::ZERO, BTreeSet::from([CHILD_REF])),
            (
                WORLD,
                INVENTORY_GRID,
                BTreeSet::from([GUARDED_REF, UNPARSED_REF, NO_MODEL_REF]),
            ),
            (
                CHILD,
                INVENTORY_GRID,
                BTreeSet::from([BLEND_REF, MISSING_MODEL_REF]),
            ),
        ] {
            let entries: Vec<_> = package.placed_references_in_cell(world, grid).collect();
            assert_eq!(
                entries.iter().map(|(id, _)| *id).collect::<BTreeSet<_>>(),
                expected_ids
            );
            assert_eq!(
                entries.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                expected_ids.into_iter().collect::<Vec<_>>()
            );
            for (id, entry) in entries {
                assert!(seen.insert(id));
                assert_eq!(Some(entry), package.placed_reference(id));
                assert_eq!(Some(entry.snapshot.clone()), authority.reference(id));
                assert_eq!(entry.snapshot.placed_worldspace_id, world);
                assert_eq!(entry.snapshot.cell, (grid.x, grid.y));
            }
        }
        assert_eq!(seen, expected);
        assert_eq!(
            package
                .placed_references_in_cell(0x999, IVec2::ZERO)
                .count(),
            0
        );
    }

    #[test]
    fn bulk_mapping_validation_matches_per_row_success_and_refusal_messages() {
        for attack in [
            "none",
            "transform",
            "missing_coverage",
            "wrong_digest",
            "malformed",
            "model",
            "texture",
        ] {
            let directory = published_inputs_with_inventory(true, true);
            let root = directory.path();
            let core = crate::cluster::overlay_fixture::base_db(root);
            let store = crate::cluster::overlay_fixture::overlay_db(root);
            match attack {
                "none" => {}
                "transform" => {
                    core.execute(
                        "UPDATE \"references\" SET pos_x=pos_x+1 WHERE id=?1",
                        [PARENT_REF],
                    )
                    .unwrap();
                }
                "missing_coverage" => {
                    store
                        .execute(
                            "DELETE FROM cluster_reference_coverage WHERE reference_id=?1",
                            [NO_MODEL_REF],
                        )
                        .unwrap();
                }
                "wrong_digest" => {
                    store.execute("UPDATE cluster_reference_coverage SET input_digest=?1 WHERE reference_id=?2", params!["ab".repeat(32), PARENT_REF]).unwrap();
                }
                "malformed" => {
                    core.execute(
                        "UPDATE \"references\" SET scale='bad' WHERE id=?1",
                        [NO_MODEL_REF],
                    )
                    .unwrap();
                }
                "model" => {
                    fs::write(root.join(MODEL), b"changed model").unwrap();
                }
                "texture" => {
                    fs::write(root.join(TEXTURE), b"changed texture").unwrap();
                }
                _ => unreachable!(),
            }
            drop((core, store));
            let overlay = crate::cluster::overlay_fixture::overlay_path(root);
            let db = crate::cluster::overlay_fixture::read_db(root);
            let (declared, builder): (String, String) = db
                .query_row(
                    "SELECT classifier_scope, builder_identity FROM cluster_provenance WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let authority = shared::cluster_recipe::authority::CoreAuthority::open_declared(
                Connection::open(root.join("skyrim_world.db")).unwrap(),
                root,
                published_files(&fs::read(root.join("conversion-manifest.json")).unwrap()).unwrap(),
                Scope::from_declared_json(&declared).unwrap(),
                builder,
            );
            if attack == "malformed" {
                // Authoritative row decoding is eager: malformed DB rows refuse construction.
                let error = authority
                    .err()
                    .expect("malformed source rows must refuse authority construction");
                assert_eq!(
                    error.to_string(),
                    "reading authoritative cluster reference rows"
                );
                assert!(
                    matches!(
                        error.downcast_ref::<rusqlite::Error>(),
                        Some(rusqlite::Error::InvalidColumnType(
                            _,
                            column,
                            rusqlite::types::Type::Text,
                        )) if column == "scale"
                    ),
                    "{error:#}"
                );
                continue;
            }
            let mut authority = authority.unwrap();
            let legacy = validate_mapping(&db, &overlay, None, &OpenAuthorityMemo::new(&authority));
            authority.preload_snapshots().unwrap();
            let bulk = validate_mapping(&db, &overlay, None, &OpenAuthorityMemo::new(&authority));
            let preloaded = authority.preloaded_authority().unwrap();
            let parallel = validate_mapping_parallel(&db, &overlay, None, &preloaded);
            assert_eq!(parallel, bulk, "parallel attack={attack}");
            if attack == "none" {
                let inputs: Vec<_> = preloaded
                    .examined_references()
                    .into_iter()
                    .map(|id| (id, authority.preloaded_reference(id).unwrap()))
                    .collect();
                // Only mapped eligible references have model paths in the index.
                let inputs: Vec<_> = inputs
                    .into_iter()
                    .filter(|(_, row)| row.candidate.is_some())
                    .collect();
                assert_eq!(
                    index_reference_facts(&inputs, &preloaded, true).unwrap(),
                    index_reference_facts(&inputs, &preloaded, false).unwrap()
                );
            }

            assert_eq!(bulk, legacy, "attack={attack}");
            assert_eq!(
                bulk.as_ref().err().map(ToString::to_string),
                legacy.as_ref().err().map(ToString::to_string)
            );
            assert_eq!(bulk.is_ok(), attack == "none", "attack={attack}");
        }
    }

    #[test]
    fn authoritative_reader_refuses_changed_core_or_input_bytes_with_unchanged_envelope() {
        for attack in ["transform", "base_mods", "model", "texture"] {
            let directory = published();
            let root = directory.path();
            let lock = AssetLock::acquire_exclusive(root).unwrap();
            match attack {
                "transform" => {
                    Connection::open(root.join("skyrim_world.db"))
                        .unwrap()
                        .execute(
                            "UPDATE \"references\" SET pos_x=pos_x+1 WHERE id=?1",
                            [PARENT_REF],
                        )
                        .unwrap();
                }
                "base_mods" => {
                    let mods = serialize_subrecords(&[(b"MODS".to_vec(), vec![1, 0, 0, 0])]);
                    Connection::open(root.join("skyrim_world.db"))
                        .unwrap()
                        .execute("UPDATE records SET data=?1 WHERE form_id=0x11", [mods])
                        .unwrap();
                }
                "model" | "texture" => {
                    let path = root.join(if attack == "model" { MODEL } else { TEXTURE });
                    let mut bytes = fs::read(&path).unwrap();
                    let last = bytes.len() - 1;
                    bytes[last] ^= 1;
                    fs::write(path, bytes).unwrap();
                }
                _ => unreachable!(),
            }
            drop(lock);
            // Core/source bytes are not base-owned: this must reach the authority.
            let precheck = envelope::open(
                root,
                &crate::cluster::overlay_fixture::overlay_path(root),
                false,
            )
            .unwrap();
            drop(precheck);
            let error = match crate::cluster::overlay_fixture::open_fixture(root, false) {
                Ok(_) => panic!("authority accepted attack {attack}"),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                error.contains("DigestNotAuthoritative") || error.contains("EligibilityMismatch"),
                "attack {attack} must reach authoritative input validation: {error}"
            );
        }
    }

    #[test]
    fn rehashed_synchronized_mapping_relabel_still_needs_authoritative_reference_inputs() {
        let directory = published();
        let root = directory.path();
        let lock = AssetLock::acquire_exclusive(root).unwrap();
        let db = crate::cluster::overlay_fixture::overlay_db(root);
        // Unknown source reference: relabel both output tables and their digest
        // consistently, then legitimately recompute the extension integrity hash.
        let digest = "ef".repeat(32);
        db.execute("UPDATE cluster_instance_map SET reference_id=0x999,input_digest=?1 WHERE reference_id=?2",params![digest,PARENT_REF]).unwrap();
        db.execute("UPDATE cluster_reference_coverage SET reference_id=0x999,input_digest=?1 WHERE reference_id=?2",params![digest,PARENT_REF]).unwrap();
        drop(db);
        crate::cluster::overlay_fixture::rehash_entries(root);
        drop(lock);
        let overlay = crate::cluster::overlay_fixture::overlay_path(root);
        let precheck = envelope::open(root, &overlay, false).unwrap();
        let db = crate::cluster::overlay_fixture::read_db(root);
        shared::cluster_mapping::validate_mapping_structure(
            &db,
            &overlay,
            Some(&precheck.core_identity),
        )
        .expect("synchronized relabel must pass the non-authoritative structure check");
        drop(db);
        drop(precheck);
        let error = match crate::cluster::overlay_fixture::open_fixture(root, false) {
            Ok(_) => panic!("authority accepted a nonexistent reference"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains("ScopeMismatch"), "{error}");
    }
}
