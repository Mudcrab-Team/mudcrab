//! GPU preparation observations for the resources owned by current streaming demand.
//!
//! IDs are weak observations. This bridge never keeps an asset alive or certifies
//! successful drawing, collision readiness, or physical GPU memory reclamation.

use bevy::{
    asset::{AssetId, UntypedAssetId, VisitAssetDependencies},
    pbr::{MaterialBindGroupAllocators, PreparedMaterial},
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        erased_render_asset::ErasedRenderAssets,
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
        texture::GpuImage,
    },
    world_serialization::WorldAsset,
};
use std::{
    any::TypeId,
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PreparationKey {
    Scene(AssetId<WorldAsset>),
    Cell(Entity),
    /// A runtime-owned resource reservation, independent of placement identity.
    Resource(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparationDemand {
    pub key: PreparationKey,
    pub meshes: Vec<AssetId<Mesh>>,
    pub images: Vec<AssetId<Image>>,
    pub materials: Vec<UntypedAssetId>,
}

/// GPU-side logical ownership, including a mesh's remaining allocator slices.
/// `None` means that the resource needed to observe absence was unavailable.
#[derive(Clone, Debug, Default)]
pub(crate) struct PreparationPresence {
    pub meshes: Option<usize>,
    pub images: Option<usize>,
    pub materials: Option<usize>,
}

impl PreparationPresence {
    /// Main-world source ownership must also be absent before releasing a
    /// reservation. This predicate alone cannot establish asset disposal.
    pub fn all_absent(&self) -> bool {
        self.meshes == Some(0) && self.images == Some(0) && self.materials == Some(0)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PreparationSnapshot {
    pub source_main_frame: u64,
    pub render_observation_seq: u64,
    pub observed_available: bool,
    /// `Some(true)` is prepared, `Some(false)` is pending, and `None` is unknown.
    pub readiness: HashMap<PreparationKey, Option<bool>>,
    pub still_present: HashMap<PreparationKey, PreparationPresence>,
    /// Counts unique demanded resources, rather than placements or whole-pack assets.
    pub pending_meshes: Option<usize>,
    pub pending_images: Option<usize>,
    pub pending_materials: Option<usize>,
    observed_demands: Arc<[PreparationDemand]>,
}

impl PreparationSnapshot {
    /// Demand IDs must be sorted and deduplicated, as they are in the submitted
    /// batch. An older observation of the same key with other IDs is unknown.
    pub fn readiness_for(&self, demand: &PreparationDemand) -> Option<bool> {
        self.observed_demand(demand)?;
        self.readiness.get(&demand.key).copied().flatten()
    }

    pub fn presence_for(&self, demand: &PreparationDemand) -> Option<&PreparationPresence> {
        self.observed_demand(demand)?;
        self.still_present.get(&demand.key)
    }

    fn observed_demand(&self, demand: &PreparationDemand) -> Option<&PreparationDemand> {
        let index = self
            .observed_demands
            .binary_search_by_key(&demand.key, |observed| observed.key)
            .ok()?;
        let observed = &self.observed_demands[index];
        (observed == demand).then_some(observed)
    }
}

#[derive(Debug, Default)]
struct DemandBatch {
    demands: Arc<[PreparationDemand]>,
    meshes: HashSet<AssetId<Mesh>>,
    images: HashSet<AssetId<Image>>,
    materials: HashSet<UntypedAssetId>,
}

impl DemandBatch {
    #[cfg(test)]
    fn new(demands: Vec<PreparationDemand>) -> Self {
        Self::from_normalized(normalized_demands(demands))
    }

    fn from_normalized(demands: Vec<PreparationDemand>) -> Self {
        let mut batch = Self::default();
        for demand in &demands {
            batch.meshes.extend(&demand.meshes);
            batch.images.extend(&demand.images);
            batch.materials.extend(&demand.materials);
        }
        batch.demands = demands.into();
        batch
    }
}

fn normalized_demands(demands: Vec<PreparationDemand>) -> Vec<PreparationDemand> {
    // A caller may submit the same key through more than one owner. Combine
    // those observations without double-counting dependencies.
    let mut merged = BTreeMap::<PreparationKey, PreparationDemand>::new();
    for demand in demands {
        let entry = merged
            .entry(demand.key)
            .or_insert_with(|| PreparationDemand {
                key: demand.key,
                meshes: Vec::new(),
                images: Vec::new(),
                materials: Vec::new(),
            });
        entry.meshes.extend(demand.meshes);
        entry.images.extend(demand.images);
        entry.materials.extend(demand.materials);
    }
    merged
        .into_values()
        .map(|mut demand| {
            normalize(&mut demand.meshes);
            normalize(&mut demand.images);
            normalize(&mut demand.materials);
            demand
        })
        .collect()
}

fn normalize<T: Ord>(ids: &mut Vec<T>) {
    ids.sort_unstable();
    ids.dedup();
}

#[derive(Clone)]
struct PreparationRequest {
    source_main_frame: u64,
    batch: Arc<DemandBatch>,
}

#[derive(Default)]
struct BridgeState {
    request: Option<PreparationRequest>,
    latest: Option<PreparationSnapshot>,
    render_available: bool,
}

#[derive(Resource, Clone, Default)]
pub(crate) struct StreamingPreparationBridge(Arc<Mutex<BridgeState>>);

impl StreamingPreparationBridge {
    pub fn has_render_world(&self) -> bool {
        self.0.lock().is_ok_and(|state| state.render_available)
    }

    pub fn replace_demands(&self, source_main_frame: u64, demands: Vec<PreparationDemand>) {
        let demands = normalized_demands(demands);
        let Ok(mut state) = self.0.lock() else { return };
        if state
            .request
            .as_ref()
            .is_some_and(|request| source_main_frame < request.source_main_frame)
        {
            return;
        }
        // Keep the precomputed unique ID sets when only the main-frame stamp
        // changed. Extracting this request then requires only an Arc clone.
        let batch = state
            .request
            .as_ref()
            .filter(|request| request.batch.demands.as_ref() == demands.as_slice())
            .map(|request| request.batch.clone())
            .unwrap_or_else(|| Arc::new(DemandBatch::from_normalized(demands)));
        if !state.render_available {
            state.latest = Some(observe_batch(
                source_main_frame,
                0,
                &batch,
                None,
                None,
                None,
            ));
        }
        state.request = Some(PreparationRequest {
            source_main_frame,
            batch,
        });
    }

    pub fn latest(&self) -> Option<PreparationSnapshot> {
        self.0.lock().ok()?.latest.clone()
    }

    /// Supply logical observations to ownership tests without creating a GPU.
    #[cfg(test)]
    pub(crate) fn publish_fixture(
        &self,
        source_main_frame: u64,
        demands: Vec<PreparationDemand>,
        ready: bool,
    ) {
        self.publish_fixture_state(source_main_frame, demands, true, ready);
    }

    #[cfg(test)]
    pub(crate) fn publish_absent_fixture(
        &self,
        source_main_frame: u64,
        demands: Vec<PreparationDemand>,
    ) {
        self.publish_fixture_state(source_main_frame, demands, false, false);
    }

    #[cfg(test)]
    fn publish_fixture_state(
        &self,
        source_main_frame: u64,
        demands: Vec<PreparationDemand>,
        present: bool,
        ready: bool,
    ) {
        self.0.lock().unwrap().render_available = true;
        let batch = DemandBatch::new(demands);
        let state = ResourceState { present, ready };
        let meshes = batch.meshes.iter().map(|id| (*id, state)).collect();
        let images = batch.images.iter().map(|id| (*id, state)).collect();
        let materials = batch.materials.iter().map(|id| (*id, state)).collect();
        self.publish(observe_batch(
            source_main_frame,
            1,
            &batch,
            Some(&meshes),
            Some(&images),
            Some(&materials),
        ));
    }

    fn request(&self) -> Option<PreparationRequest> {
        self.0.lock().ok()?.request.clone()
    }

    fn publish(&self, snapshot: PreparationSnapshot) {
        let Ok(mut state) = self.0.lock() else { return };
        if state.latest.as_ref().is_some_and(|latest| {
            (snapshot.source_main_frame, snapshot.render_observation_seq)
                <= (latest.source_main_frame, latest.render_observation_seq)
        }) {
            return;
        }
        state.latest = Some(snapshot);
    }
}

pub(crate) struct StreamingPreparationPlugin;

impl Plugin for StreamingPreparationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StreamingPreparationBridge>();
        let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            if let Ok(mut state) = bridge.0.lock() {
                state.render_available = true;
            }
            render_app
                .insert_resource(bridge)
                .init_resource::<ExtractedDemand>()
                .add_systems(ExtractSchedule, extract_demands)
                .add_systems(
                    Render,
                    observe_preparation.after(RenderSystems::PrepareBindGroups),
                );
        }
    }
}

#[derive(Resource, Default)]
struct ExtractedDemand {
    request: Option<PreparationRequest>,
    sequence: u64,
}

fn extract_demands(
    bridge: Extract<Res<StreamingPreparationBridge>>,
    mut extracted: ResMut<ExtractedDemand>,
) {
    extracted.request = bridge.request();
}

/// Inspect a CPU-loaded scene once, before its placements are activated. The
/// caller must wait for recursive CPU dependencies to finish before caching it.
pub(crate) fn scene_dependencies_available(
    scene: &WorldAsset,
    materials: &Assets<StandardMaterial>,
) -> bool {
    scene.world.iter_entities().all(|entity| {
        entity
            .get::<MeshMaterial3d<StandardMaterial>>()
            .is_none_or(|material| materials.contains(material.0.id()))
    })
}

/// Collect weak IDs only after [`scene_dependencies_available`] succeeds.
pub(crate) fn scene_demand(
    key: PreparationKey,
    scene: &WorldAsset,
    materials: &Assets<StandardMaterial>,
) -> PreparationDemand {
    let mut demand = PreparationDemand {
        key,
        meshes: Vec::new(),
        images: Vec::new(),
        materials: Vec::new(),
    };
    for entity in scene.world.iter_entities() {
        if let Some(mesh) = entity.get::<Mesh3d>() {
            demand.meshes.push(mesh.0.id());
        }
        if let Some(material) = entity.get::<MeshMaterial3d<StandardMaterial>>() {
            demand.materials.push(material.0.id().into());
        }
    }
    normalize(&mut demand.meshes);
    normalize(&mut demand.materials);
    for id in &demand.materials {
        if let Some(material) = materials.get(id.typed_debug_checked::<StandardMaterial>()) {
            material.visit_dependencies(&mut |dependency| {
                if dependency.type_id() == TypeId::of::<Image>() {
                    demand.images.push(dependency.typed_debug_checked());
                }
            });
        }
    }
    normalize(&mut demand.images);
    demand
}

#[derive(Clone, Copy)]
struct ResourceState {
    present: bool,
    ready: bool,
}

type MeshStates = HashMap<AssetId<Mesh>, ResourceState>;
type ImageStates = HashMap<AssetId<Image>, ResourceState>;
type MaterialStates = HashMap<UntypedAssetId, ResourceState>;

fn observe_preparation(
    bridge: Res<StreamingPreparationBridge>,
    mut extracted: ResMut<ExtractedDemand>,
    meshes: Option<Res<RenderAssets<RenderMesh>>>,
    allocator: Option<Res<MeshAllocator>>,
    images: Option<Res<RenderAssets<GpuImage>>>,
    materials: Option<Res<ErasedRenderAssets<PreparedMaterial>>>,
    bind_groups: Option<Res<MaterialBindGroupAllocators>>,
) {
    let Some(request) = extracted.request.clone() else {
        return;
    };
    let mesh_states = meshes
        .as_ref()
        .zip(allocator.as_ref())
        .map(|(meshes, allocator)| {
            request
                .batch
                .meshes
                .iter()
                .map(|id| {
                    let mesh = meshes.get(*id);
                    let vertices = allocator.mesh_vertex_slice(id).map(|slice| slice.range);
                    let indices = allocator.mesh_index_slice(id).map(|slice| slice.range);
                    let present = mesh.is_some() || vertices.is_some() || indices.is_some();
                    let ready = mesh.is_some_and(|mesh| {
                        resident_ranges_match(
                            mesh.vertex_count,
                            match mesh.buffer_info {
                                RenderMeshBufferInfo::Indexed { count, .. } => Some(count),
                                RenderMeshBufferInfo::NonIndexed => None,
                            },
                            vertices,
                            indices,
                        )
                    });
                    (*id, ResourceState { present, ready })
                })
                .collect::<MeshStates>()
        });
    let image_states = images.as_ref().map(|images| {
        request
            .batch
            .images
            .iter()
            .map(|id| {
                let ready = images.get(*id).is_some();
                (
                    *id,
                    ResourceState {
                        present: ready,
                        ready,
                    },
                )
            })
            .collect::<ImageStates>()
    });
    let material_states =
        materials
            .as_ref()
            .zip(bind_groups.as_ref())
            .map(|(materials, allocators)| {
                request
                    .batch
                    .materials
                    .iter()
                    .map(|id| {
                        let material = materials.get(*id);
                        let ready = material.is_some_and(|material| {
                            allocators
                                .get(&id.type_id())
                                .and_then(|allocator| allocator.get(material.binding.group))
                                .is_some_and(|slab| slab.bind_group().is_some())
                        });
                        (
                            *id,
                            ResourceState {
                                present: material.is_some(),
                                ready,
                            },
                        )
                    })
                    .collect::<MaterialStates>()
            });
    extracted.sequence = extracted.sequence.saturating_add(1);
    bridge.publish(observe_batch(
        request.source_main_frame,
        extracted.sequence,
        &request.batch,
        mesh_states.as_ref(),
        image_states.as_ref(),
        material_states.as_ref(),
    ));
}

fn observe_batch(
    source_main_frame: u64,
    render_observation_seq: u64,
    batch: &DemandBatch,
    meshes: Option<&MeshStates>,
    images: Option<&ImageStates>,
    materials: Option<&MaterialStates>,
) -> PreparationSnapshot {
    let observed_available = meshes.is_some() && images.is_some() && materials.is_some();
    let mut snapshot = PreparationSnapshot {
        source_main_frame,
        render_observation_seq,
        observed_available,
        pending_meshes: meshes.map(|states| states.values().filter(|state| !state.ready).count()),
        pending_images: images.map(|states| states.values().filter(|state| !state.ready).count()),
        pending_materials: materials
            .map(|states| states.values().filter(|state| !state.ready).count()),
        observed_demands: batch.demands.clone(),
        ..default()
    };
    for demand in batch.demands.iter() {
        snapshot.readiness.insert(
            demand.key,
            meshes
                .zip(images)
                .zip(materials)
                .map(|((meshes, images), materials)| {
                    all_ready(&demand.meshes, meshes)
                        && all_ready(&demand.images, images)
                        && all_ready(&demand.materials, materials)
                }),
        );
        snapshot.still_present.insert(
            demand.key,
            PreparationPresence {
                meshes: meshes.map(|states| present_count(&demand.meshes, states)),
                images: images.map(|states| present_count(&demand.images, states)),
                materials: materials.map(|states| present_count(&demand.materials, states)),
            },
        );
    }
    snapshot
}

fn all_ready<K: Eq + std::hash::Hash>(ids: &[K], states: &HashMap<K, ResourceState>) -> bool {
    ids.iter()
        .all(|id| states.get(id).is_some_and(|state| state.ready))
}

fn present_count<K: Eq + std::hash::Hash>(ids: &[K], states: &HashMap<K, ResourceState>) -> usize {
    ids.iter()
        .filter(|id| states.get(*id).is_some_and(|state| state.present))
        .count()
}

fn resident_ranges_match(
    vertex_count: u32,
    indexed_count: Option<u32>,
    vertex_range: Option<Range<u32>>,
    index_range: Option<Range<u32>>,
) -> bool {
    if !vertex_range
        .and_then(|range| range.end.checked_sub(range.start))
        .is_some_and(|count| count >= vertex_count)
    {
        return false;
    }
    match indexed_count {
        None | Some(0) => true,
        Some(count) => index_range
            .and_then(|range| range.end.checked_sub(range.start))
            .is_some_and(|available| available >= count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty(key: PreparationKey) -> PreparationDemand {
        PreparationDemand {
            key,
            meshes: vec![],
            images: vec![],
            materials: vec![],
        }
    }

    fn key() -> PreparationKey {
        PreparationKey::Cell(Entity::PLACEHOLDER)
    }

    #[test]
    fn headless_observation_never_reports_vacuous_readiness_or_confirmed_release() {
        let mut app = App::new();
        app.add_plugins(StreamingPreparationPlugin);
        let bridge = app.world().resource::<StreamingPreparationBridge>();
        assert!(!bridge.has_render_world());
        bridge.replace_demands(9, vec![empty(key())]);
        let result = bridge.latest().unwrap();
        assert_eq!(result.source_main_frame, 9);
        assert!(!result.observed_available);
        assert_eq!(result.readiness[&key()], None);
        assert!(!result.still_present[&key()].all_absent());
        assert_eq!(result.pending_meshes, None);
    }

    #[test]
    fn empty_loaded_scene_is_ready_when_observation_resources_are_available() {
        let batch = DemandBatch::new(vec![empty(key())]);
        let snapshot = observe_batch(
            1,
            1,
            &batch,
            Some(&default()),
            Some(&default()),
            Some(&default()),
        );
        assert_eq!(snapshot.readiness[&key()], Some(true));
        assert!(snapshot.still_present[&key()].all_absent());
        assert_eq!(snapshot.pending_meshes, Some(0));
    }

    #[test]
    fn shared_dependencies_count_once_but_every_owner_waits() {
        let mut assets = Assets::<Mesh>::default();
        let mesh = assets.add(Cuboid::default());
        let mut world = World::new();
        let other = PreparationKey::Cell(world.spawn_empty().id());
        let demand = PreparationDemand {
            meshes: vec![mesh.id(), mesh.id()],
            ..empty(key())
        };
        let batch = DemandBatch::new(vec![
            demand.clone(),
            demand,
            PreparationDemand {
                meshes: vec![mesh.id()],
                ..empty(other)
            },
        ]);
        let pending = HashMap::from([(
            mesh.id(),
            ResourceState {
                present: true,
                ready: false,
            },
        )]);
        let result = observe_batch(
            3,
            1,
            &batch,
            Some(&pending),
            Some(&default()),
            Some(&default()),
        );
        assert_eq!(batch.demands.len(), 2);
        assert_eq!(result.pending_meshes, Some(1));
        assert_eq!(result.readiness[&key()], Some(false));
        assert_eq!(result.readiness[&other], Some(false));
        assert_eq!(result.still_present[&key()].meshes, Some(1));
    }

    #[test]
    fn material_bind_group_pending_is_distinct_from_material_absence() {
        let mut assets = Assets::<StandardMaterial>::default();
        let material = assets.add(StandardMaterial::default());
        let batch = DemandBatch::new(vec![PreparationDemand {
            materials: vec![material.id().into()],
            ..empty(key())
        }]);
        let states = HashMap::from([(
            material.id().into(),
            ResourceState {
                present: true,
                ready: false,
            },
        )]);
        let snapshot = observe_batch(
            4,
            1,
            &batch,
            Some(&default()),
            Some(&default()),
            Some(&states),
        );
        assert_eq!(snapshot.readiness[&key()], Some(false));
        assert_eq!(snapshot.pending_materials, Some(1));
        assert!(!snapshot.still_present[&key()].all_absent());
    }

    #[test]
    fn extracted_scene_lists_unique_meshes_and_all_material_image_dependencies() {
        let mut meshes = Assets::<Mesh>::default();
        let mut images = Assets::<Image>::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mesh = meshes.add(Cuboid::default());
        let color = images.add(Image::default());
        let normal = images.add(Image::default());
        let material = materials.add(StandardMaterial {
            base_color_texture: Some(color.clone()),
            emissive_texture: Some(color.clone()),
            normal_map_texture: Some(normal.clone()),
            ..default()
        });
        let mut scene_world = World::new();
        for _ in 0..2 {
            scene_world.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
        }
        let scene = WorldAsset::new(scene_world);
        assert!(scene_dependencies_available(&scene, &materials));
        let demand = scene_demand(key(), &scene, &materials);
        assert_eq!(demand.meshes, vec![mesh.id()]);
        assert_eq!(demand.materials, vec![material.id().untyped()]);
        assert_eq!(demand.images.len(), 2);
        assert!(demand.images.contains(&color.id()));
        assert!(demand.images.contains(&normal.id()));
        materials.remove(material.id());
        assert!(!scene_dependencies_available(&scene, &materials));
    }

    #[test]
    fn bridge_reuses_unique_id_sets_and_ignores_stale_requests_and_results() {
        let bridge = StreamingPreparationBridge::default();
        bridge.0.lock().unwrap().render_available = true;
        assert!(bridge.has_render_world());
        bridge.replace_demands(10, vec![empty(key())]);
        let original = bridge.request().unwrap().batch;
        bridge.replace_demands(11, vec![empty(key())]);
        assert!(Arc::ptr_eq(&original, &bridge.request().unwrap().batch));
        bridge.replace_demands(8, vec![]);
        assert_eq!(bridge.request().unwrap().source_main_frame, 11);
        bridge.publish(PreparationSnapshot {
            source_main_frame: 10,
            render_observation_seq: 2,
            ..default()
        });
        bridge.publish(PreparationSnapshot {
            source_main_frame: 9,
            render_observation_seq: 3,
            ..default()
        });
        bridge.publish(PreparationSnapshot {
            source_main_frame: 10,
            render_observation_seq: 1,
            ..default()
        });
        assert_eq!(bridge.latest().unwrap().source_main_frame, 10);
        assert_eq!(bridge.latest().unwrap().render_observation_seq, 2);
    }

    #[test]
    fn resource_reservations_with_reused_dependencies_keep_separate_identity() {
        let first = PreparationKey::Resource(1);
        let second = PreparationKey::Resource(2);
        let batch = DemandBatch::new(vec![empty(first), empty(second)]);
        let snapshot = observe_batch(
            3,
            1,
            &batch,
            Some(&default()),
            Some(&default()),
            Some(&default()),
        );
        assert_eq!(snapshot.readiness.len(), 2);
        assert_eq!(snapshot.readiness[&first], Some(true));
        assert_eq!(snapshot.readiness[&second], Some(true));
    }

    #[test]
    fn an_empty_set_observation_cannot_prepare_or_release_a_later_nonempty_set() {
        let demand = empty(key());
        let batch = DemandBatch::new(vec![demand.clone()]);
        let snapshot = observe_batch(
            3,
            1,
            &batch,
            Some(&default()),
            Some(&default()),
            Some(&default()),
        );
        assert_eq!(snapshot.readiness_for(&demand), Some(true));
        assert!(snapshot.presence_for(&demand).unwrap().all_absent());
        let mut assets = Assets::<Mesh>::default();
        let mesh = assets.add(Cuboid::default());
        let changed = PreparationDemand {
            meshes: vec![mesh.id()],
            ..demand
        };
        assert_eq!(snapshot.readiness_for(&changed), None);
        assert!(snapshot.presence_for(&changed).is_none());
    }

    #[test]
    fn mesh_descriptor_without_full_allocator_ranges_is_not_ready() {
        assert!(!resident_ranges_match(4, Some(6), None, Some(0..6)));
        assert!(!resident_ranges_match(4, Some(6), Some(0..4), Some(0..5)));
        assert!(!resident_ranges_match(
            4,
            None,
            Some(Range { start: 10, end: 8 }),
            None
        ));
        assert!(resident_ranges_match(4, Some(6), Some(0..4), Some(0..6)));
        assert!(resident_ranges_match(4, None, Some(0..4), None));
    }
}
