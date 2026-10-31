//! Changed-only loading proof over every applicable cluster consumer.

use super::*;
use crate::cluster::loading::{ClusterLoadingGate, ClusterWorldReadiness};
use bevy::ecs::change_detection::Tick;
use bevy::pbr::{ExtractedPointLight, PointAndSpotLightViewEntities, ViewLightEntities};
use bevy::render::MainWorld;

#[path = "readiness_aggregate.rs"]
mod aggregate;

#[path = "readiness_inputs.rs"]
mod inputs;

// UI creates an ExtractedView sharing its camera's MainEntity, but no world
// geometry role. Identify roles independently of their phases/GPU proof: missing
// required Camera3d/light proof must remain a blocking consumer.
type LoadingViews<'w, 's> =
    Query<'w, 's, SelectionViewState, Or<(With<Camera3d>, With<LightEntity>)>>;

#[derive(Clone)]
struct Consumer {
    key: RetainedViewEntity,
    world: u32,
    shadow: bool,
    layers: RenderLayers,
    readiness: Option<Arc<Vec<bool>>>,
    readiness_changes: Option<Arc<ReadinessChanges>>,
}

impl Consumer {
    fn same(&self, other: &Self) -> bool {
        self.key == other.key
            && self.world == other.world
            && self.shadow == other.shadow
            && self.layers == other.layers
            && match (&self.readiness, &other.readiness) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NeededBatch {
    batch: usize,
    terrain: bool,
    invalid_owned_state: bool,
}

/// Inventory inputs only. Loading generations and source qualification belong to the aggregate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NeededCatalogKey {
    scene_changed: Tick,
    refusal_revision: Option<u64>,
    layout_scope: u64,
    runtime_state_revision: Option<u64>,
}

/// Render readiness can change without changing any placed instance. In
/// particular, global material uploads and GPU packing never own the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadinessKey {
    catalog: NeededCatalogKey,
    gpu_identity: u64,
    gpu_generation: u64,
    gpu_layout: u64,
    materials_revision: u64,
}

struct Snapshot {
    package: Arc<Certificate>,
    inventory: Arc<Inventory>,
    layout: u64,
    generation: u64,
    frame: u64,
    worlds: std::collections::HashMap<u32, ClusterWorldReadiness>,
}

#[derive(Resource, Default)]
struct Producer {
    generation: u64,
    revision: Option<[u64; 9]>,
    needed_key: Option<NeededCatalogKey>,
    observation_key: Option<ReadinessKey>,
    needed_catalog: NeededCatalog,
    inventory: Option<Arc<Inventory>>,
    inventory_scene_changed: Option<Tick>,
    inventory_valid: bool,
    contributions: Option<InventoryContributions>,
    aggregate: aggregate::Aggregate,
    batch_inputs: inputs::BatchInputs,
    material_reader: bevy::render::erased_render_asset::ErasedRenderAssetChangeReader,
    catalog_dirty: std::collections::BTreeSet<usize>,
    force_publication: bool,
    #[cfg(test)]
    inventory_work: InventoryWork,
    #[cfg(test)]
    validation_walks: usize,
    consumers: Vec<Consumer>,
    frame: u64,
    pending: Option<Snapshot>,
    withdrawal_pending: bool,
    #[cfg(test)]
    walks: usize,
}

type NeededCatalog = HashMap<u32, HashMap<(usize, bool), NeededBatch>>;
type Inventory = crate::cluster::instance_inventory::InstanceInventory;
type Certificate = super::super::super::reader::CertifiedPackage;
type RecordKey = crate::cluster::reader::RecordKey;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InventoryWork {
    rebuilt_batches: u64,
    visited_instances: u64,
}

impl InventoryWork {
    fn record(self) {
        #[cfg(test)]
        crate::work_counters::record_count(
            "cluster_drawability/rebuilt_batches",
            self.rebuilt_batches,
        );
        #[cfg(test)]
        crate::work_counters::record_count(
            "cluster_drawability/visited_instances",
            self.visited_instances,
        );
    }
}

/// Keys removed before each changed batch's new contributions are installed.
struct InventoryContributions {
    inventory: Arc<Inventory>,
    refusal_revision: Option<u64>,
    runtime_revision: Option<u64>,
    refused: std::collections::BTreeSet<RecordKey>,
    runtime: std::collections::BTreeSet<u32>,
    groups: Vec<Vec<(u32, bool)>>,
}

impl Producer {
    /// Resource ticks are local to the render world; only this ordered validation
    /// uses them. Publication compares retained allocation provenance instead.
    fn validate_inventory(
        &mut self,
        inventory: &Arc<Inventory>,
        package: &Arc<Certificate>,
        layout: u64,
        scene_changed: Tick,
        instances: &[ClusterInstance],
    ) -> bool {
        let replaced = self
            .inventory
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, inventory));
        if replaced || self.inventory_scene_changed != Some(scene_changed) {
            self.inventory = Some(inventory.clone());
            self.inventory_scene_changed = Some(scene_changed);
            self.inventory_valid = inventory.matches_instances(instances);
            #[cfg(test)]
            {
                self.validation_walks += 1;
            }
            if replaced {
                self.needed_key = None;
                self.force_publication = true;
            }
        }
        inventory.matches(package, layout) && self.inventory_valid
    }

    fn withdraw(&mut self) {
        self.withdrawal_pending |= self.revision.take().is_some();
        self.pending = None;
        self.aggregate = default();
        self.batch_inputs = default();
        self.material_reader = default();
    }

    fn update_inventory(
        &mut self,
        inventory: &Arc<Inventory>,
        package: &Arc<Certificate>,
        refusal_revision: Option<u64>,
        refused: &std::collections::BTreeSet<RecordKey>,
        runtime_revision: Option<u64>,
        runtime_references: impl FnOnce() -> std::collections::BTreeSet<u32>,
    ) -> bool {
        let reset = self
            .contributions
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(&old.inventory, inventory));
        let mut touched = std::collections::BTreeSet::new();
        if reset {
            if let Some(old) = &self.contributions {
                self.catalog_dirty.extend(0..old.inventory.batch_count());
            }
            self.needed_catalog.clear();
            touched.extend((0..inventory.batch_count()).map(|batch| batch as u32));
            self.contributions = Some(InventoryContributions {
                inventory: inventory.clone(),
                refusal_revision,
                runtime_revision,
                refused: refused.clone(),
                runtime: runtime_references(),
                groups: vec![Vec::new(); inventory.batch_count()],
            });
        } else if let Some(state) = self.contributions.as_mut() {
            if state.refusal_revision != refusal_revision {
                for record in state.refused.symmetric_difference(refused) {
                    touched.extend(inventory.batches_for_record(record));
                }
                state.refused = refused.clone();
                state.refusal_revision = refusal_revision;
            }
            if state.runtime_revision != runtime_revision {
                let runtime = runtime_references();
                for reference in state.runtime.symmetric_difference(&runtime) {
                    touched.extend(inventory.batches_for_reference(*reference));
                }
                state.runtime = runtime;
                state.runtime_revision = runtime_revision;
            }
        }
        let mut work = InventoryWork::default();
        if let Some(state) = self.contributions.as_mut() {
            for batch in touched {
                work.rebuilt_batches += 1;
                let mut previous = HashMap::new();
                for (world, terrain) in state.groups[batch as usize].drain(..) {
                    if let std::collections::hash_map::Entry::Occupied(mut entry) =
                        self.needed_catalog.entry(world)
                    {
                        if let Some(need) = entry.get_mut().remove(&(batch as usize, terrain)) {
                            previous.insert((world, terrain), need);
                        }
                        if entry.get().is_empty() {
                            entry.remove();
                        }
                    }
                }
                let mut groups: HashMap<(u32, bool), NeededBatch> = HashMap::new();
                for &index in inventory.indices_for_batch(batch) {
                    work.visited_instances += 1;
                    let (identity, _) = inventory.instances()[index];
                    let (world, terrain, invalid) = match identity {
                        ClusterInstanceIdentity::Object(record) => {
                            if state.refused.contains(&record) {
                                continue;
                            }
                            // Inventory::build proved every object key against this retained,
                            // immutable certificate before admission.
                            let Some(record) = package.record(&record) else {
                                error!(
                                    ?record,
                                    "validated cluster inventory lost its object certificate"
                                );
                                work.record();
                                self.inventory_valid = false;
                                self.contributions = None;
                                self.needed_catalog.clear();
                                return false;
                            };
                            (
                                record.worldspace,
                                false,
                                state.runtime.contains(&record.reference),
                            )
                        }
                        ClusterInstanceIdentity::Terrain { worldspace, .. } => {
                            (worldspace, true, false)
                        }
                        ClusterInstanceIdentity::Fixture => continue,
                    };
                    groups
                        .entry((world, terrain))
                        .or_insert(NeededBatch {
                            batch: batch as usize,
                            terrain,
                            invalid_owned_state: false,
                        })
                        .invalid_owned_state |= invalid;
                }
                if groups != previous {
                    self.catalog_dirty.insert(batch as usize);
                }
                for ((world, terrain), need) in groups {
                    self.needed_catalog
                        .entry(world)
                        .or_default()
                        .insert((batch as usize, terrain), need);
                    state.groups[batch as usize].push((world, terrain));
                }
            }
        }
        work.record();
        #[cfg(test)]
        {
            self.inventory_work.rebuilt_batches += work.rebuilt_batches;
            self.inventory_work.visited_instances += work.visited_instances;
        }
        true
    }
}

impl Producer {
    #[cfg(test)]
    fn observe(
        &mut self,
        inputs: ReadinessKey,
        revision: [u64; 9],
        consumers: Vec<Consumer>,
        needed: impl FnOnce() -> NeededCatalog,
        batch_ready: impl Fn(usize) -> (bool, bool, bool, bool),
    ) -> Option<HashMap<u32, ClusterWorldReadiness>> {
        self.observe_with(
            inputs,
            revision,
            consumers,
            |producer| {
                producer.needed_catalog = needed();
                true
            },
            batch_ready,
        )
    }

    // Fixture compatibility: old callers expose only broad notifications. They
    // conservatively name all catalog batches; production supplies exact deltas.
    #[cfg(test)]
    fn observe_with(
        &mut self,
        inputs: ReadinessKey,
        revision: [u64; 9],
        consumers: Vec<Consumer>,
        needed: impl FnOnce(&mut Self) -> bool,
        batch_ready: impl Fn(usize) -> (bool, bool, bool, bool),
    ) -> Option<HashMap<u32, ClusterWorldReadiness>> {
        let changed = self.needed_key != Some(inputs.catalog)
            || self.observation_key != Some(inputs)
            || self.revision != Some(revision)
            || self.consumers.len() != consumers.len()
            || !self
                .consumers
                .iter()
                .zip(&consumers)
                .all(|(a, b)| a.same(b));
        if !changed {
            return None;
        }
        let touched = self
            .needed_catalog
            .values()
            .flat_map(|world| world.values().map(|need| need.batch))
            .collect();
        self.observe_incremental(
            inputs,
            revision,
            consumers,
            |producer| {
                let old: Vec<_> = producer
                    .needed_catalog
                    .values()
                    .flat_map(|world| world.values().map(|need| need.batch))
                    .collect();
                if !needed(producer) {
                    return false;
                }
                producer.catalog_dirty.extend(old);
                producer.catalog_dirty.extend(
                    producer
                        .needed_catalog
                        .values()
                        .flat_map(|world| world.values().map(|need| need.batch)),
                );
                true
            },
            touched,
            true,
            |_, batch| batch_ready(batch),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_incremental(
        &mut self,
        inputs: ReadinessKey,
        revision: [u64; 9],
        consumers: Vec<Consumer>,
        needed: impl FnOnce(&mut Self) -> bool,
        mut changed: std::collections::BTreeSet<usize>,
        force: bool,
        batch_ready: impl Fn(&inputs::BatchInputs, usize) -> (bool, bool, bool, bool),
    ) -> Option<HashMap<u32, ClusterWorldReadiness>> {
        if self.needed_key != Some(inputs.catalog) {
            #[cfg(test)]
            let previous = self.needed_key;
            #[cfg(test)]
            for (name, changed) in [
                ("cluster_drawability/catalog_initial", previous.is_none()),
                (
                    "cluster_drawability/catalog_scene",
                    previous.is_some_and(|old| old.scene_changed != inputs.catalog.scene_changed),
                ),
                (
                    "cluster_drawability/catalog_refusal",
                    previous
                        .is_some_and(|old| old.refusal_revision != inputs.catalog.refusal_revision),
                ),
                (
                    "cluster_drawability/catalog_layout",
                    previous.is_some_and(|old| old.layout_scope != inputs.catalog.layout_scope),
                ),
                (
                    "cluster_drawability/catalog_runtime_state",
                    previous.is_some_and(|old| {
                        old.runtime_state_revision != inputs.catalog.runtime_state_revision
                    }),
                ),
            ] {
                if changed {
                    #[cfg(test)]
                    crate::work_counters::record(name);
                }
            }
            {
                if !needed(self) {
                    self.withdraw();
                    return None;
                }
            }
            self.needed_key = Some(inputs.catalog);
            #[cfg(test)]
            crate::work_counters::record("cluster_drawability/needed_catalog");
        }
        changed.append(&mut self.catalog_dirty);
        let new_gpu = self
            .observation_key
            .is_some_and(|old| old.gpu_identity != inputs.gpu_identity);
        if new_gpu {
            self.aggregate = default();
        }
        if self.revision.is_some_and(|old| old[6] != revision[6]) {
            changed.extend(
                self.needed_catalog
                    .values()
                    .flat_map(|world| world.values().map(|need| need.batch)),
            );
        }
        // Broad source/material epochs are observation metadata, never batch
        // dependencies. Scope and admission boundaries require fresh publication.
        let provenance_changed = std::mem::take(&mut self.force_publication);
        let force = force
            || provenance_changed
            || self.revision.is_none_or(|old| old[..4] != revision[..4])
            || new_gpu;
        let result = self.aggregate.observe(
            consumers.clone(),
            &self.needed_catalog,
            &changed,
            |batch| batch_ready(&self.batch_inputs, batch),
            force,
        );
        #[cfg(test)]
        if result.is_some() {
            self.walks += 1;
        }
        self.revision = Some(revision);
        self.observation_key = Some(inputs);
        self.consumers = consumers;
        result
    }
}

fn complete_camera_consumers(consumers: &mut Vec<Consumer>, expected: &HashMap<MainEntity, u32>) {
    for (&main, &world) in expected {
        if !consumers
            .iter()
            .any(|consumer| !consumer.shadow && consumer.key.main_entity == main)
        {
            consumers.push(Consumer {
                key: RetainedViewEntity::new(main, None, 0),
                world,
                shadow: false,
                layers: RenderLayers::from_layers(crate::render::REFLECTION_VIEW_LAYERS),
                readiness: None,
                readiness_changes: None,
            });
        }
    }
}

/// View existence alone is not a world-scoped drawing proof. Missing or foreign
/// consumers remain explicit dependencies until selection can draw this world.
fn require_shadow_consumer(
    consumers: &mut Vec<Consumer>,
    key: RetainedViewEntity,
    world: u32,
    layers: &RenderLayers,
) {
    if !consumers
        .iter()
        .any(|consumer| consumer.shadow && consumer.key == key && consumer.world == world)
    {
        consumers.push(Consumer {
            key,
            world,
            shadow: true,
            layers: layers.clone(),
            readiness: None,
            readiness_changes: None,
        });
    }
}

fn extract_generation(
    mut producer: ResMut<Producer>,
    gate: Extract<Option<Res<ClusterLoadingGate>>>,
) {
    producer.generation = gate.as_ref().map_or(0, |gate| gate.generation());
}

#[allow(clippy::too_many_arguments)]
fn observe(
    gpu: Option<ResMut<ClusterGpu>>,
    scene: Option<Res<ClusterScene>>,
    sources: Option<ResMut<Sources>>,
    frame: Option<Res<ClusterFrame>>,
    pipelines: Option<Res<ClusterPipelines>>,
    cache: Option<Res<PipelineCache>>,
    materials: Option<Res<ErasedRenderAssets<PreparedMaterial>>>,
    runtime_state: Option<Res<crate::cluster::runtime_state::RuntimeReferenceState>>,
    views: LoadingViews,
    camera_lights: Query<&ViewLightEntities>,
    local_lights: Query<(
        &MainEntity,
        &ExtractedPointLight,
        &PointAndSpotLightViewEntities,
    )>,
    mut producer: ResMut<Producer>,
) {
    let (
        Some(mut gpu),
        Some(scene),
        Some(mut sources),
        Some(frame),
        Some(pipelines),
        Some(cache),
        Some(materials),
    ) = (gpu, scene, sources, frame, pipelines, cache, materials)
    else {
        producer.withdraw();
        return;
    };
    let (Some(package), Some(layout)) = (gpu.certificate.clone(), sources.lazy_layout_scope) else {
        producer.withdraw();
        return;
    };
    let Some(inventory) = scene.inventory.as_ref() else {
        producer.withdraw();
        return;
    };
    // An inventory describes identities only within the exact retained certificate
    // and final batch layout. GPU identity itself is aggregate drawing proof.
    let coherent = sources
        .certificate
        .as_ref()
        .is_some_and(|source| Arc::ptr_eq(source, &package))
        && scene
            .certificate
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, &package));
    if !coherent
        || !producer.validate_inventory(
            inventory,
            &package,
            layout,
            scene.last_changed(),
            &scene.instances,
        )
    {
        producer.withdraw();
        return;
    }
    let compute = prepared_cluster_cull(&gpu, &frame, &pipelines, &cache).is_some();
    let mut consumers: Vec<_> = views
        .iter()
        .filter_map(|(_, view, light, layers)| {
            let key = view.retained_view_entity;
            let world = sources
                .worlds
                .get(&key.main_entity)
                .or_else(|| sources.worlds.get(&key.auxiliary_entity))
                .copied()
                .flatten()?;
            Some(Consumer {
                key,
                world,
                shadow: light.is_some(),
                layers: layers.cloned().unwrap_or_default(),
                readiness: gpu
                    .views
                    .get(&key)
                    .and_then(|view| view.readiness_cache.as_ref())
                    .map(|(_, ready)| ready.clone()),
                readiness_changes: gpu
                    .views
                    .get(&key)
                    .and_then(|view| view.readiness_changes.clone()),
            })
        })
        .collect();
    // Enumerate expected shadow views independently of the views that arrived.
    // Omitting a missing cascade/point face must never make the proof vacuously ready.
    for (entity, camera, light, layers) in &views {
        if light.is_some() {
            continue;
        }
        let Some(world) = sources
            .worlds
            .get(&camera.retained_view_entity.main_entity)
            .copied()
            .flatten()
        else {
            continue;
        };
        let layers = layers.cloned().unwrap_or_default();
        // Bevy owns the selected/capped/atlas-allocated cascade set. Its actual
        // required view list is authoritative, not the raw configured cascade count.
        match camera_lights.get(entity) {
            Ok(required) => {
                for (index, shadow_view) in required.lights.iter().enumerate() {
                    if views.get(*shadow_view).is_err() {
                        consumers.push(Consumer {
                            key: RetainedViewEntity::new(
                                camera.retained_view_entity.main_entity,
                                Some(camera.retained_view_entity.main_entity),
                                index as u32,
                            ),
                            world,
                            shadow: true,
                            layers: layers.clone(),
                            readiness: None,
                            readiness_changes: None,
                        });
                    }
                }
            }
            Err(_) => consumers.push(Consumer {
                key: camera.retained_view_entity,
                world,
                shadow: true,
                layers: layers.clone(),
                readiness: None,
                readiness_changes: None,
            }),
        }
        match sources
            .light_dependencies
            .get(&camera.retained_view_entity.main_entity)
        {
            Some(Some(dependencies)) => {
                for dependency in dependencies {
                    let Some((main, light, shadow_views)) =
                        local_lights.iter().find(|(main, _, _)| *main == dependency)
                    else {
                        consumers.push(Consumer {
                            key: RetainedViewEntity::new(*dependency, None, 0),
                            world,
                            shadow: true,
                            layers: layers.clone(),
                            readiness: None,
                            readiness_changes: None,
                        });
                        continue;
                    };
                    if !light.shadow_maps_enabled || shadow_views.is_empty() {
                        continue;
                    }
                    let expected = if light.spot_light_angles.is_some() {
                        1
                    } else {
                        bevy::camera::primitives::CUBE_MAP_FACES.len()
                    };
                    for face in 0..expected {
                        let key = shadow_views
                            .get(face)
                            .and_then(|entity| views.get(*entity).ok())
                            .map_or_else(
                                || RetainedViewEntity::new(*main, None, face as u32),
                                |(_, view, _, _)| view.retained_view_entity,
                            );
                        require_shadow_consumer(&mut consumers, key, world, &layers);
                    }
                }
            }
            _ => consumers.push(Consumer {
                key: camera.retained_view_entity,
                world,
                shadow: true,
                layers,
                readiness: None,
                readiness_changes: None,
            }),
        }
    }
    complete_camera_consumers(&mut consumers, &sources.expected_cameras);
    consumers.sort_unstable_by_key(|consumer| {
        (
            consumer.key.main_entity,
            consumer.key.auxiliary_entity,
            consumer.key.subview_index,
            consumer.world,
            consumer.shadow,
        )
    });
    let revision = [
        producer.generation,
        gpu.identity,
        gpu.generation,
        gpu.layout_epoch,
        sources.version,
        sources.layout_version,
        u64::from(compute),
        layout,
        runtime_state.as_ref().map_or(0, |state| state.revision),
    ];
    let inputs = ReadinessKey {
        catalog: NeededCatalogKey {
            scene_changed: scene.last_changed(),
            refusal_revision: sources.refusal_revision,
            layout_scope: layout,
            runtime_state_revision: runtime_state.as_ref().map(|state| state.revision),
        },
        gpu_identity: gpu.identity,
        gpu_generation: gpu.generation,
        gpu_layout: gpu.layout_epoch,
        materials_revision: materials.revision(),
    };
    let source_changes = std::mem::take(&mut sources.readiness_changes.batches);
    let binding_changes = std::mem::take(&mut gpu.readiness_material_batches);
    let material_changes = materials.read_changes(&mut producer.material_reader);
    let changed = producer.batch_inputs.refresh(
        gpu.identity,
        &sources.readiness_changes.scope,
        gpu.batches.len(),
        &binding_changes,
        &source_changes,
        material_changes.as_deref(),
        |batch| gpu.batches[batch].material,
        |batch| {
            (
                sources.templates.get(batch) == Some(&true),
                sources.source_fallback_batches.contains(&(batch as u32)),
            )
        },
        |asset| {
            materials
                .get(asset)
                .is_none_or(|material| material.properties.shadows_enabled)
        },
    );
    let result = producer.observe_incremental(
        inputs,
        revision,
        consumers,
        |producer| {
            producer.update_inventory(
                inventory,
                &package,
                sources.refusal_revision,
                &sources.permanently_refused_records,
                runtime_state.as_ref().map(|state| state.revision),
                || {
                    runtime_state
                        .as_ref()
                        .into_iter()
                        .flat_map(|state| state.readiness_references())
                        .collect()
                },
            )
        },
        changed,
        false,
        |facts, batch| {
            let (template, failed, casts_shadows) = facts
                .facts
                .get(batch)
                .copied()
                .unwrap_or((false, false, true));
            (compute, template, failed, casts_shadows)
        },
    );
    if let Some(worlds) = result {
        // Attribute missing dependencies only when the aggregate changes. These are
        // observations, never permission to omit an expected consumer.
        for consumer in producer
            .consumers
            .iter()
            .filter(|consumer| consumer.readiness.is_none())
        {
            let extracted = views
                .iter()
                .find(|(_, view, _, _)| view.retained_view_entity == consumer.key);
            debug!(
                key = ?consumer.key,
                world = consumer.world,
                shadow = consumer.shadow,
                layers = ?consumer.layers,
                expected_camera = sources.expected_cameras.contains_key(&consumer.key.main_entity),
                gpu_view = gpu.views.contains_key(&consumer.key),
                extracted_entity = ?extracted.as_ref().map(|(entity, _, _, _)| entity),
                extracted_shadow = ?extracted.as_ref().map(|(_, _, light, _)| light.is_some()),
                directional_views = ?extracted.as_ref().map(|(entity, _, _, _)| {
                    camera_lights.get(*entity).ok().map(|lights| lights.lights.len())
                }),
                local_light_domain = ?sources.light_dependencies.get(&consumer.key.main_entity)
                    .map(|lights| lights.as_ref().map(HashSet::len)),
                "cluster loading missing consumer"
            );
        }
        producer.withdrawal_pending = false;
        producer.frame = producer.frame.wrapping_add(1);
        producer.pending = Some(Snapshot {
            package: package.clone(),
            inventory: inventory.clone(),
            layout,
            generation: producer.generation,
            frame: producer.frame,
            worlds,
        });
    }
}

fn publish(mut producer: ResMut<Producer>, mut main: ResMut<MainWorld>) {
    let Some(Snapshot {
        package,
        inventory,
        layout,
        generation,
        frame,
        worlds,
    }) = producer.pending.take()
    else {
        if std::mem::take(&mut producer.withdrawal_pending)
            && let Some(mut gate) = main.get_resource_mut::<ClusterLoadingGate>()
        {
            gate.withdraw();
        }
        return;
    };
    let coherent = main
        .get_resource::<crate::cluster::package::ValidatedPackage>()
        .is_some_and(|current| Arc::ptr_eq(&current.0, &package))
        && main
            .get_resource::<crate::cluster::package::LazyBatches>()
            .is_some_and(|lazy| lazy.layout_scope == layout)
        && inventory.matches(&package, layout)
        && main.get_resource::<ClusterScene>().is_some_and(|scene| {
            scene
                .certificate
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &package))
                && scene
                    .inventory
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &inventory))
        });
    // These are the renderer's already-computed per-world aggregates, not a
    // second batch discovery pass. Rejected scope/generation/frame facts cannot refuse.
    let failed = worlds.iter().find_map(|(&world, facts)| {
        (facts.failed_batches > 0).then_some((world, facts.failed_batches))
    });
    let mut terminal = None;
    if let Some(mut gate) = main.get_resource_mut::<ClusterLoadingGate>() {
        let accepted = coherent && gate.publish(&package, layout, generation, frame, worlds);
        if accepted {
            terminal = failed;
        }
        if !coherent {
            gate.withdraw();
        }
        if !accepted {
            // Rejection must not suppress recomputation when this exact Arc returns.
            producer.revision = None;
        }
    } else {
        producer.revision = None;
    }
    if let Some((world, failed)) = terminal {
        crate::streaming::failure::refuse_loading(
            &mut main,
            format!(
                "cluster loading refused: {failed} required batch/view failure(s) in world {world} at accepted renderer publication"
            ),
        );
    }
}

pub(super) fn install(app: &mut App) {
    if let Some(render) = app.get_sub_app_mut(RenderApp) {
        render
            .init_resource::<Producer>()
            .add_systems(ExtractSchedule, (publish, extract_generation).chain())
            .add_systems(
                Render,
                observe
                    .after(select_instances)
                    .after(RenderSystems::PrepareBindGroups)
                    .before(RenderSystems::Render),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    include!("runtime_policy_tests.rs");
    include!("readiness_inventory_tests.rs");
    include!("readiness_batch_tests.rs");
    include!("readiness_delta_tests.rs");
    fn catalog_key() -> ReadinessKey {
        ReadinessKey {
            catalog: NeededCatalogKey {
                scene_changed: Tick::new(5),
                refusal_revision: Some(23),
                layout_scope: 29,
                runtime_state_revision: Some(31),
            },
            gpu_identity: 11,
            gpu_generation: 13,
            gpu_layout: 17,
            materials_revision: 19,
        }
    }

    pub(super) fn consumer(id: u32, shadow: bool, ready: Option<Arc<Vec<bool>>>) -> Consumer {
        Consumer {
            key: RetainedViewEntity {
                main_entity: Entity::from_raw_u32(id).unwrap().into(),
                auxiliary_entity: Entity::PLACEHOLDER.into(),
                subview_index: 0,
            },
            world: 42,
            shadow,
            layers: crate::render::PLACED_OBJECT_RENDER_LAYERS,
            readiness: ready,
            readiness_changes: None,
        }
    }
    fn extracted_view(main: MainEntity, subview: u32) -> ExtractedView {
        ExtractedView {
            retained_view_entity: RetainedViewEntity::new(main, None, subview),
            clip_from_view: Mat4::IDENTITY,
            world_from_view: GlobalTransform::IDENTITY,
            clip_from_world: None,
            target_format: bevy::render::render_resource::TextureFormat::Rgba16Float,
            viewport: UVec4::new(0, 0, 64, 64),
            color_grading: default(),
            invert_culling: false,
        }
    }

    #[test]
    fn loading_views_exclude_ui_subviews_and_keep_all_world_shadow_roles_without_phases() {
        use bevy::ecs::system::SystemState;
        let mut world = World::new();
        let main = Entity::from_raw_u32(100).unwrap().into();
        let camera = world
            .spawn((Camera3d::default(), extracted_view(main, 0)))
            .id();
        // Bevy UI creates a separate ExtractedView sharing the camera's main
        // entity. It has no Camera3d or light role and never draws world geometry.
        let ui = world.spawn(extracted_view(main, 1)).id();
        let light = Entity::from_raw_u32(101).unwrap();
        let mut expected = HashSet::from([camera]);
        for (subview, role) in [
            LightEntity::Directional {
                light_entity: light,
                cascade_index: 0,
            },
            LightEntity::Point {
                light_entity: light,
                face_index: 0,
            },
            LightEntity::Spot {
                light_entity: light,
            },
        ]
        .into_iter()
        .enumerate()
        {
            expected.insert(
                world
                    .spawn((role, extracted_view(light.into(), subview as u32)))
                    .id(),
            );
        }
        let mut state = SystemState::<LoadingViews>::new(&mut world);
        let views = state.get(&world).unwrap();
        let actual: HashSet<_> = views.iter().map(|(entity, _, _, _)| entity).collect();
        assert!(
            !actual.contains(&ui),
            "UI inherited a world consumer role from its camera"
        );
        assert_eq!(
            actual, expected,
            "camera/shadow roles remain required before phases arrive"
        );
    }

    fn needed() -> NeededCatalog {
        HashMap::from([(
            42,
            HashMap::from([(
                (0, false),
                NeededBatch {
                    batch: 0,
                    terrain: false,
                    invalid_owned_state: false,
                },
            )]),
        )])
    }
    #[test]
    fn requested_prepasses_remain_required_when_their_phase_is_missing() {
        use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass};
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        let depth = world.spawn(DepthPrepass).id();
        let normal = world.spawn(NormalPrepass).id();
        let motion = world.spawn(MotionVectorPrepass).id();
        let plain = world.spawn_empty().id();
        world.init_resource::<ViewBinnedRenderPhases<Opaque3dPrepass>>();
        let required = world
            .run_system_once(
                move |required: RequiredPrepassViews,
                      phases: Res<ViewBinnedRenderPhases<Opaque3dPrepass>>| {
                    assert!(phases.is_empty());
                    [depth, normal, motion, plain].map(|entity| required.contains(entity))
                },
            )
            .unwrap();
        assert_eq!(required, [true, true, true, false]);
    }

    #[test]
    fn unparented_local_lights_inherit_only_a_unique_dependent_camera_world() {
        let camera: MainEntity = Entity::from_raw_u32(1).unwrap().into();
        let other: MainEntity = Entity::from_raw_u32(2).unwrap().into();
        let light: MainEntity = Entity::from_raw_u32(9).unwrap().into();
        let mut worlds = HashMap::from([(camera, Some(42))]);
        let mut dependencies = HashMap::from([(camera, Some(HashSet::from([light])))]);
        complete_light_worlds(&mut worlds, &dependencies);
        assert_eq!(worlds.get(&light), Some(&Some(42)));
        worlds.remove(&light);
        worlds.insert(other, Some(43));
        dependencies.insert(other, Some(HashSet::from([light])));
        complete_light_worlds(&mut worlds, &dependencies);
        assert_eq!(worlds.get(&light), Some(&None));
        worlds.insert(light, Some(7));
        complete_light_worlds(&mut worlds, &dependencies);
        assert_eq!(
            worlds.get(&light),
            Some(&Some(7)),
            "authored world ownership wins"
        );
    }

    #[test]
    fn local_shadow_view_without_a_world_consumer_is_missing_even_if_the_view_exists() {
        let key = RetainedViewEntity::new(Entity::from_raw_u32(9).unwrap().into(), None, 3);
        let mut consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        require_shadow_consumer(&mut consumers, key, 42, &RenderLayers::default());
        require_shadow_consumer(&mut consumers, key, 42, &RenderLayers::default());
        let mut producer = Producer::default();
        let result = producer
            .observe(catalog_key(), [0; 9], consumers, needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        assert_eq!(result[&42].missing_views, 1);
        assert_eq!(result[&42].pending_pipelines, 1);
    }

    #[test]
    fn missing_expected_camera_blocks_another_ready_camera_in_the_same_world() {
        let missing = Entity::from_raw_u32(2).unwrap().into();
        let mut consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        complete_camera_consumers(&mut consumers, &HashMap::from([(missing, 42)]));
        let mut producer = Producer::default();
        let result = producer
            .observe(catalog_key(), [0; 9], consumers, needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        assert_eq!(result[&42].applicable_cameras, 2);
        assert_eq!(result[&42].missing_views, 1);
        assert_eq!(result[&42].pending_pipelines, 1);
    }

    #[test]
    fn ordinary_world_and_unlayered_shadows_keep_owned_objects_and_terrain() {
        for (layers, shadow) in [
            (
                RenderLayers::from_layers(crate::render::REFLECTION_VIEW_LAYERS),
                false,
            ),
            (RenderLayers::default(), true),
        ] {
            let domain = ViewDomain::new(&layers, shadow);
            assert!(domain.objects && domain.terrain);
        }
        let placed = ViewDomain::new(&crate::render::PLACED_OBJECT_RENDER_LAYERS, false);
        assert!(placed.objects);
        assert!(!placed.terrain);
        let unrelated = ViewDomain::new(&RenderLayers::none(), false);
        assert!(!unrelated.objects && !unrelated.terrain);
    }

    #[test]
    fn all_views_are_required_and_settled_observations_do_not_walk_batches() {
        let mut producer = Producer::default();
        let ready = Arc::new(vec![true]);
        let consumers = vec![
            consumer(1, false, Some(ready.clone())),
            consumer(2, true, None),
        ];
        let waiting = producer
            .observe(catalog_key(), [0; 9], consumers.clone(), needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        assert_eq!(waiting[&42].missing_views, 1);
        assert_eq!(waiting[&42].pending_pipelines, 1);
        assert!(
            producer
                .observe(
                    catalog_key(),
                    [0; 9],
                    consumers,
                    || panic!("settled batch scan"),
                    |_| panic!("settled readiness scan")
                )
                .is_none()
        );
        let ready_world = producer
            .observe(
                catalog_key(),
                [0; 9],
                vec![
                    consumer(1, false, Some(ready.clone())),
                    consumer(2, true, Some(ready)),
                ],
                needed,
                |_| (true, true, false, true),
            )
            .unwrap();
        assert_eq!(ready_world[&42].pending_pipelines, 0);
        assert_eq!(producer.walks, 2);
    }
    #[test]
    fn source_qualification_changes_refresh_readiness_without_rebuilding_the_needed_catalog() {
        let mut producer = Producer::default();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        let catalog_calls = std::cell::Cell::new(0);
        let catalog = || {
            catalog_calls.set(catalog_calls.get() + 1);
            needed()
        };
        let mut revision = [1, 11, 13, 17, 19, 23, 1, 29, 31];
        let initial = producer
            .observe(catalog_key(), revision, consumers.clone(), catalog, |_| {
                (true, true, false, true)
            })
            .unwrap();
        assert_eq!(initial[&42].pending_templates, 0);
        assert_eq!(initial[&42].failed_batches, 0);
        revision[4] += 1; // Source qualification changed; inventory did not.
        revision[5] += 1; // Source template layout qualification changed too.
        let changed = producer
            .observe(catalog_key(), revision, consumers.clone(), catalog, |_| {
                (true, false, true, true)
            })
            .expect("changed source qualification must publish fresh readiness");
        assert_eq!(changed[&42].needed_batches, 1);
        assert_eq!(changed[&42].pending_templates, 1);
        assert_eq!(changed[&42].failed_batches, 1);
        assert_eq!(
            catalog_calls.get(),
            1,
            "source qualification changes must reuse the needed catalog"
        );
        revision[4] += 1;
        let recovered = producer
            .observe(catalog_key(), revision, consumers, catalog, |_| {
                (true, true, false, true)
            })
            .unwrap();
        assert_eq!(recovered[&42].pending_templates, 0);
        assert_eq!(recovered[&42].failed_batches, 0);
        assert_eq!(catalog_calls.get(), 1);
    }

    #[test]
    fn changed_consumers_recheck_readiness_without_rebuilding_the_needed_catalog() {
        let mut producer = Producer::default();
        producer
            .observe(
                catalog_key(),
                [0; 9],
                vec![consumer(1, false, Some(Arc::new(vec![true])))],
                needed,
                |_| (true, true, false, true),
            )
            .unwrap();
        let changed = producer
            .observe(
                catalog_key(),
                [0; 9],
                vec![
                    consumer(1, false, Some(Arc::new(vec![false]))),
                    consumer(2, true, None),
                ],
                || panic!("consumer changes must reuse the needed catalog"),
                |_| (true, true, false, true),
            )
            .unwrap();
        assert_eq!(changed[&42].applicable_cameras, 1);
        assert_eq!(changed[&42].needed_batches, 2);
        assert_eq!(changed[&42].missing_views, 1);
        assert_eq!(changed[&42].pending_pipelines, 2);
    }

    #[test]
    fn new_admission_generation_republishes_without_rebuilding_the_needed_catalog() {
        let mut producer = Producer::default();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        let mut revision = [0; 9];
        producer
            .observe(catalog_key(), revision, consumers.clone(), needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        revision[0] = 1;
        let current = producer
            .observe(
                catalog_key(),
                revision,
                consumers,
                || panic!("admission generation does not change the inventory"),
                |_| (true, true, false, true),
            )
            .expect("a new admission generation must not replay old proof");
        assert_eq!(current[&42].pending_pipelines, 0);
        assert_eq!(current[&42].needed_batches, 1);
    }

    #[test]
    fn prepared_material_changes_refresh_proof_without_rewalking_instances() {
        let mut producer = Producer::default();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        let mut key = catalog_key();
        producer
            .observe(key, [0; 9], consumers.clone(), needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        key.materials_revision += 1;
        let current = producer
            .observe(
                key,
                [0; 9],
                consumers,
                || panic!("prepared materials do not change the instance inventory"),
                |_| (true, true, false, true),
            )
            .expect("material changes must still refresh the drawing proof");
        assert_eq!(current[&42].needed_batches, 1);
    }

    #[test]
    fn unchanged_catalog_and_snapshot_inputs_skip_both_callbacks() {
        let mut producer = Producer::default();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        producer
            .observe(catalog_key(), [0; 9], consumers.clone(), needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        assert!(
            producer
                .observe(
                    catalog_key(),
                    [0; 9],
                    consumers,
                    || panic!("unchanged inventory must not be walked"),
                    |_| panic!("unchanged aggregate must not be recomputed"),
                )
                .is_none()
        );
    }

    #[test]
    fn changed_inventory_scope_replaces_the_needed_catalog() {
        let base = catalog_key();
        let mut changes = [base; 4];
        changes[0].catalog.scene_changed = Tick::new(6);
        changes[1].catalog.refusal_revision = Some(24);
        changes[2].catalog.layout_scope += 1;
        changes[3].catalog.runtime_state_revision = Some(32);
        for key in changes {
            let mut producer = Producer::default();
            let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
            producer
                .observe(base, [0; 9], consumers.clone(), needed, |_| {
                    (true, true, false, true)
                })
                .unwrap();
            let calls = std::cell::Cell::new(0);
            let changed = producer
                .observe(
                    key,
                    [0; 9],
                    consumers,
                    || {
                        calls.set(calls.get() + 1);
                        let mut catalog = needed();
                        catalog
                            .get_mut(&42)
                            .unwrap()
                            .get_mut(&(0, false))
                            .unwrap()
                            .invalid_owned_state = true;
                        catalog
                    },
                    |_| (true, true, false, true),
                )
                .expect("catalog changes must defeat an otherwise exact aggregate replay");
            assert_eq!(calls.get(), 1, "key={key:?}");
            assert_eq!(changed[&42].failed_batches, 1, "key={key:?}");
        }
    }

    #[test]
    fn optional_catalog_revisions_distinguish_absence_from_revision_zero() {
        for refusal in [true, false] {
            let mut producer = Producer::default();
            let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
            let mut key = catalog_key();
            let calls = std::cell::Cell::new(0);
            for (expected, value) in [None, Some(0), Some(1)].into_iter().enumerate() {
                if refusal {
                    key.catalog.refusal_revision = value;
                } else {
                    key.catalog.runtime_state_revision = value;
                }
                let result = producer.observe(
                    key,
                    [0; 9],
                    consumers.clone(),
                    || {
                        calls.set(calls.get() + 1);
                        needed()
                    },
                    |_| (true, true, false, true),
                );
                assert!(result.is_some(), "refusal={refusal}, revision={value:?}");
                assert_eq!(calls.get(), expected + 1);
            }
        }
    }

    #[test]
    fn changed_material_shadow_policy_replaces_the_cached_shadow_requirement() {
        let mut producer = Producer::default();
        let ready = Arc::new(vec![true]);
        let consumers = vec![
            consumer(1, false, Some(ready.clone())),
            consumer(2, true, Some(ready)),
        ];
        let mut key = catalog_key();
        let initial = producer
            .observe(key, [0; 9], consumers.clone(), needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        assert_eq!(initial[&42].needed_batches, 2);
        key.materials_revision += 1;
        let changed = producer
            .observe(
                key,
                [0; 9],
                consumers,
                || panic!("shadow policy changes readiness, not instance inventory"),
                |_| (true, true, false, false),
            )
            .unwrap();
        assert_eq!(changed[&42].applicable_cameras, 1);
        assert_eq!(changed[&42].needed_batches, 1);
        assert_eq!(changed[&42].pending_pipelines, 0);
    }

    #[test]
    fn new_permanent_refusal_removes_old_catalog_failure_contributions() {
        let mut producer = Producer::default();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        let mut key = catalog_key();
        let initial = producer
            .observe(key, [0; 9], consumers.clone(), needed, |_| {
                (true, false, true, true)
            })
            .unwrap();
        assert_eq!(initial[&42].needed_batches, 1);
        assert_eq!(initial[&42].failed_batches, 1);
        key.catalog.refusal_revision = Some(24);
        let changed = producer
            .observe(key, [0; 9], consumers, HashMap::new, |_| {
                panic!("a permanently refused batch is no longer needed")
            })
            .unwrap();
        assert_eq!(changed[&42].applicable_cameras, 1);
        assert_eq!(changed[&42].needed_batches, 0);
        assert_eq!(changed[&42].pending_templates, 0);
        assert_eq!(changed[&42].failed_batches, 0);
    }

    #[test]
    fn withdrawn_proof_requires_a_fresh_aggregate_even_with_the_same_needed_catalog() {
        use bevy::ecs::system::RunSystemOnce;
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let lazy = crate::cluster::package::LazyBatches::unrequested_for_test(1, Vec3::ZERO);
        let layout = lazy.layout_scope;
        let mut main = MainWorld::default();
        main.insert_resource(crate::cluster::package::ValidatedPackage(package.clone()));
        main.insert_resource(lazy);
        let inventory = install_empty_inventory_scene(&mut main, &package, layout);
        main.insert_resource(crate::config::EngineConfig {
            worldspace_id: 42,
            ..default()
        });
        main.init_resource::<crate::streaming::ActiveSpace>();
        main.init_resource::<ClusterLoadingGate>();
        let generation = main.resource::<ClusterLoadingGate>().generation();
        let mut render = World::new();
        render.insert_resource(main);
        render.init_resource::<Producer>();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
        let worlds = render
            .resource_mut::<Producer>()
            .observe(catalog_key(), [0; 9], consumers.clone(), needed, |_| {
                (true, true, false, true)
            })
            .unwrap();
        render.resource_mut::<Producer>().pending = Some(Snapshot {
            package: package.clone(),
            inventory: inventory.clone(),
            layout,
            generation,
            frame: 1,
            worlds,
        });
        render.run_system_once(publish).unwrap();
        assert!(
            render
                .resource::<MainWorld>()
                .resource::<ClusterLoadingGate>()
                .world_ready(42)
        );
        // This is the producer state when observe loses its GPU/scene/scope.
        {
            let mut producer = render.resource_mut::<Producer>();
            assert!(producer.revision.take().is_some());
            producer.pending = None;
            producer.withdrawal_pending = true;
        }
        render.run_system_once(publish).unwrap();
        assert!(
            !render
                .resource::<MainWorld>()
                .resource::<ClusterLoadingGate>()
                .world_ready(42)
        );
        let restored = render
            .resource_mut::<Producer>()
            .observe(
                catalog_key(),
                [0; 9],
                consumers,
                || panic!("same-scope return may reuse inventory, never readiness proof"),
                |_| (true, true, false, true),
            )
            .expect("withdrawal must force a fresh aggregate before readiness can return");
        assert_eq!(restored[&42].needed_batches, 1);
        assert_eq!(restored[&42].pending_pipelines, 0);
        assert!(
            !render
                .resource::<MainWorld>()
                .resource::<ClusterLoadingGate>()
                .world_ready(42),
            "recomputation alone is not published render proof"
        );
        render.resource_mut::<Producer>().pending = Some(Snapshot {
            package,
            inventory: inventory.clone(),
            layout,
            generation,
            frame: 2,
            worlds: restored,
        });
        render.run_system_once(publish).unwrap();
        assert!(
            render
                .resource::<MainWorld>()
                .resource::<ClusterLoadingGate>()
                .world_ready(42)
        );
    }

    #[test]
    fn forced_owned_failure_is_reported_without_any_stock_action() {
        let mut producer = Producer::default();
        let positive = producer
            .observe(
                catalog_key(),
                [0; 9],
                vec![consumer(1, false, Some(Arc::new(vec![true])))],
                needed,
                |_| (true, true, false, true),
            )
            .unwrap();
        assert_eq!(positive[&42].pending_pipelines, 0);
        let failed = producer
            .observe(
                catalog_key(),
                [0; 9],
                vec![consumer(1, false, Some(Arc::new(vec![false])))],
                needed,
                |_| (true, true, false, true),
            )
            .unwrap();
        assert_eq!(failed[&42].pending_pipelines, 1);
    }

    #[test]
    fn forced_owned_failure_publishes_the_counter_and_no_render_bodies() {
        use bevy::ecs::system::RunSystemOnce;
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let lazy = crate::cluster::package::LazyBatches::unrequested_for_test(1, Vec3::ZERO);
        let layout = lazy.layout_scope;
        let mut main = MainWorld::default();
        main.insert_resource(crate::cluster::package::ValidatedPackage(package.clone()));
        main.insert_resource(lazy);
        let inventory = install_empty_inventory_scene(&mut main, &package, layout);
        main.insert_resource(crate::config::EngineConfig {
            worldspace_id: 42,
            ..default()
        });
        main.init_resource::<crate::streaming::ActiveSpace>();
        main.init_resource::<ClusterLoadingGate>();
        let generation = main.resource::<ClusterLoadingGate>().generation();
        let mut render = World::new();
        render.insert_resource(main);
        render.init_resource::<Producer>();
        for (frame, pipelines) in [(1, 0), (2, 1), (3, 0)] {
            render.resource_mut::<Producer>().pending = Some(Snapshot {
                package: package.clone(),
                inventory: inventory.clone(),
                layout,
                generation,
                frame,
                worlds: std::collections::HashMap::from([(
                    42,
                    ClusterWorldReadiness {
                        applicable_cameras: 1,
                        needed_batches: 1,
                        pending_pipelines: pipelines,
                        ..default()
                    },
                )]),
            });
            render.run_system_once(publish).unwrap();
            let main = render.resource::<MainWorld>();
            assert_eq!(
                main.resource::<ClusterLoadingGate>().world_ready(42),
                pipelines == 0
            );
            assert_eq!(
                main.resource::<ClusterLoadingGate>().owned_not_drawable(42),
                pipelines
            );
            assert_eq!(
                main.iter_entities()
                    .filter(|entity| entity.contains::<Mesh3d>())
                    .count(),
                0
            );
        }
    }

    #[test]
    fn failed_snapshot_rejected_for_old_package_layout_inventory_or_generation_has_no_terminal_effect()
     {
        use crate::{
            cluster::package::{LazyBatches, ValidatedPackage},
            streaming::failure::LoadingRefusal,
        };
        use bevy::ecs::system::RunSystemOnce;
        // Scope rejection belongs to the real publisher. Gate::publish alone intentionally
        // accepts a changed package/layout after the producer establishes coherence.
        for stale in ["package", "layout", "inventory", "generation", "frame"] {
            let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
            let package = Arc::new(
                crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
            );
            let lazy = LazyBatches::unrequested_for_test(1, Vec3::ZERO);
            let layout = lazy.layout_scope;
            let mut main = MainWorld::default();
            main.insert_resource(lazy);
            main.insert_resource(ValidatedPackage(package.clone()));
            main.insert_resource(Messages::<AppExit>::default());
            main.init_resource::<ClusterLoadingGate>();
            let inventory = install_empty_inventory_scene(&mut main, &package, layout);
            let generation = main.resource::<ClusterLoadingGate>().generation();
            let ready = ClusterWorldReadiness {
                applicable_cameras: 1,
                needed_batches: 1,
                ..default()
            };
            assert!(main.resource_mut::<ClusterLoadingGate>().publish(
                &package,
                layout,
                generation,
                10,
                HashMap::from([(42, ready)]),
            ));
            let failed = ClusterWorldReadiness {
                failed_batches: 1,
                ..ready
            };
            let mut snapshot = Snapshot {
                package: package.clone(),
                inventory: inventory.clone(),
                layout,
                generation,
                frame: 11,
                worlds: HashMap::from([(42, failed)]),
            };
            match stale {
                "package" => main.insert_resource(ValidatedPackage(Arc::new(
                    crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
                ))),
                "layout" => {
                    main.resource_mut::<LazyBatches>().layout_scope += 1;
                }
                "inventory" => {
                    main.resource_mut::<ClusterScene>().inventory = Some(Arc::new(
                        Inventory::build(package.clone(), layout, &[], 0).unwrap(),
                    ));
                }
                "generation" => {
                    snapshot.generation = generation.wrapping_sub(1);
                }
                "frame" => {
                    snapshot.frame = 9;
                }
                _ => unreachable!(),
            }
            let mut exits = main.resource::<Messages<AppExit>>().get_cursor_current();
            let mut render = World::new();
            render.insert_resource(main);
            render.init_resource::<Producer>();
            render.resource_mut::<Producer>().revision = Some([0; 9]);
            render.resource_mut::<Producer>().pending = Some(snapshot);
            render.run_system_once(publish).unwrap();
            {
                let mut main = render.resource_mut::<MainWorld>();
                main.flush();
                assert!(!main.contains_resource::<LoadingRefusal>(), "stale={stale}");
                assert_eq!(
                    exits.read(main.resource::<Messages<AppExit>>()).count(),
                    0,
                    "stale={stale}"
                );
                if matches!(stale, "generation" | "frame") {
                    assert!(main.resource::<ClusterLoadingGate>().world_ready(42));
                } else {
                    assert!(!main.resource::<ClusterLoadingGate>().world_ready(42));
                }
            }
            assert!(
                render.resource::<Producer>().revision.is_none(),
                "rejection must rearm {stale}"
            );
        }
    }

    #[test]
    fn accepted_failed_publication_refuses_once_without_any_cover_or_new_demand() {
        use crate::{
            cluster::package::{LazyBatches, ValidatedPackage},
            streaming::failure::{LoadingRefusal, refuse_loading},
        };
        use bevy::ecs::system::RunSystemOnce;
        for earlier_reason in [None, Some("original terminal cell diagnostic")] {
            let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
            let package = Arc::new(
                crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
            );
            let lazy = LazyBatches::unrequested_for_test(1, Vec3::ZERO);
            let layout = lazy.layout_scope;
            let mut main = MainWorld::default();
            main.insert_resource(lazy);
            main.insert_resource(ValidatedPackage(package.clone()));
            main.insert_resource(Messages::<AppExit>::default());
            main.init_resource::<ClusterLoadingGate>();
            let inventory = install_empty_inventory_scene(&mut main, &package, layout);
            let generation = main.resource::<ClusterLoadingGate>().generation();
            let mut exits = main.resource::<Messages<AppExit>>().get_cursor_current();
            if let Some(reason) = earlier_reason {
                refuse_loading(&mut main, reason.into());
            }
            let mut render = World::new();
            render.insert_resource(main);
            render.init_resource::<Producer>();
            for frame in 1..=3 {
                render.resource_mut::<Producer>().pending = Some(Snapshot {
                    package: package.clone(),
                    inventory: inventory.clone(),
                    layout,
                    generation,
                    frame,
                    worlds: HashMap::from([(
                        42,
                        ClusterWorldReadiness {
                            applicable_cameras: 1,
                            needed_batches: 1,
                            failed_batches: 1,
                            ..default()
                        },
                    )]),
                });
                render.run_system_once(publish).unwrap();
                let mut main = render.resource_mut::<MainWorld>();
                main.flush();
                assert!(!main.resource::<ClusterLoadingGate>().world_ready(42));
                let refusal = main.get_resource::<LoadingRefusal>();
                assert!(
                    refusal.is_some(),
                    "the accepted publisher must escalate failure even with no loading transition"
                );
                if let Some(reason) = earlier_reason {
                    assert_eq!(refusal.unwrap().0, reason);
                }
                let emitted: Vec<_> = exits
                    .read(main.resource::<Messages<AppExit>>())
                    .cloned()
                    .collect();
                if frame == 1 {
                    assert_eq!(emitted, [AppExit::error()]);
                } else {
                    assert!(
                        emitted.is_empty(),
                        "accepted replay must not emit another error exit"
                    );
                }
            }
        }
    }

    #[test]
    fn actual_cached_lazy_zero_writer_cannot_erase_an_accepted_terminal_render_failure() {
        use crate::{
            cluster::package::{
                LazyBatches, ValidatedPackage, register_lazy_batch_system_for_test,
            },
            streaming::{StreamingMetrics, failure::LoadingRefusal},
        };
        use bevy::{asset::AssetPlugin, ecs::system::RunSystemOnce};
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let lazy = LazyBatches::unrequested_for_test(3, Vec3::ZERO);
        let layout = lazy.layout_scope;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<WorldAsset>()
            .init_asset::<StandardMaterial>()
            .init_resource::<crate::cluster::render::ClusterBatchHandles>()
            .init_resource::<StreamingMetrics>()
            .insert_resource(crate::config::EngineConfig {
                clusters: Some("selected".into()),
                cluster_own_statics: true,
                worldspace_id: 42,
                ..default()
            })
            .insert_resource(lazy)
            .insert_resource(ValidatedPackage(package.clone()))
            .init_resource::<ClusterLoadingGate>();
        app.update();
        let mut main = MainWorld::default();
        std::mem::swap(&mut *main, app.world_mut());
        let inventory = install_empty_inventory_scene(&mut main, &package, layout);
        let lazy_system = register_lazy_batch_system_for_test(&mut main);
        main.run_system(lazy_system).unwrap(); // Warm the actual private producer's Local cache.
        assert_eq!(
            main.resource::<StreamingMetrics>().failed_cluster_batches,
            0
        );
        let generation = main.resource::<ClusterLoadingGate>().generation();
        let mut exits = main.resource::<Messages<AppExit>>().get_cursor_current();
        let mut render = World::new();
        render.insert_resource(main);
        render.init_resource::<Producer>();
        for frame in 1..=2 {
            render.resource_mut::<Producer>().pending = Some(Snapshot {
                package: package.clone(),
                inventory: inventory.clone(),
                layout,
                generation,
                frame,
                worlds: HashMap::from([(
                    42,
                    ClusterWorldReadiness {
                        applicable_cameras: 1,
                        needed_batches: 1,
                        failed_batches: 1,
                        ..default()
                    },
                )]),
            });
            render.run_system_once(publish).unwrap();
            let mut main = render.resource_mut::<MainWorld>();
            let reason = main
                .get_resource::<LoadingRefusal>()
                .expect("the actual accepted renderer publication must commit terminal failure")
                .0
                .clone();
            assert!(!reason.is_empty());
            // This sentinel proves that the following zero comes from the real cached lazy
            // writer, not from a test-only policy consumer or another aggregate metric owner.
            main.resource_mut::<StreamingMetrics>()
                .failed_cluster_batches = 99;
            main.run_system(lazy_system).unwrap();
            assert_eq!(
                main.resource::<StreamingMetrics>().failed_cluster_batches,
                0
            );
            assert_eq!(main.resource::<LoadingRefusal>().0, reason);
            assert!(!main.resource::<ClusterLoadingGate>().world_ready(42));
            let emitted: Vec<_> = exits
                .read(main.resource::<Messages<AppExit>>())
                .cloned()
                .collect();
            if frame == 1 {
                assert_eq!(emitted, [AppExit::error()]);
            } else {
                assert!(
                    emitted.is_empty(),
                    "cached lazy work cannot re-emit terminal failure"
                );
            }
            assert!(
                main.resource::<LazyBatches>().failed_batches().is_empty(),
                "renderer terminal policy must not forge a lazy-loader failure to keep a metric nonzero"
            );
        }
    }
}
