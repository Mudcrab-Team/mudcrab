//! Same-frame destination handoff over the representation selected before stock queueing.
//! This publishes proof only; it never changes a queue, mask or draw at this late point.
use super::*;
use crate::swap_bridge::{
    destination::*,
    systems::{
        DestinationSystems, ExtractedRequests, PhaseMembers, QueuePhases,
        extract_destination_protocol,
    },
};
use bevy::{
    pbr::{ExtractedPointLight, PointAndSpotLightViewEntities, ViewLightEntities},
    render::MainWorld,
};
type LocalShadowLights<'w, 's> = Query<
    'w,
    's,
    (
        &'static MainEntity,
        &'static ExtractedPointLight,
        &'static PointAndSpotLightViewEntities,
    ),
>;

#[derive(Default, Resource)]
struct Published {
    keys: HashSet<DestinationKey>,
}

pub(super) fn install(app: &mut App) {
    app.insert_resource(RequiredHandoff);
    let Some(render) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render
        .init_resource::<Published>()
        .init_resource::<CandidateObserver>()
        .add_systems(
            ExtractSchedule,
            observe_candidates
                .after(extract_sources)
                .after(extract_batch_materials)
                .after(extract_destination_protocol),
        )
        .add_systems(
            Render,
            publish
                .in_set(DestinationSystems::PublishHandoff)
                .after(finalize_terrain_clips)
                .after(RenderSystems::PrepareBindGroups)
                .before(DestinationSystems::Evaluate)
                .before(RenderSystems::Render),
        );
}

/// Eligibility/composition changes invalidate old reports; becoming drawable does
/// not invalidate its own new Ready report on every pipeline-preparation step.
#[path = "destination_scope.rs"]
mod destination_scope;

#[derive(Default, Resource)]
struct CandidateObserver {
    versions: Option<(u64, u64)>,
    /// The request set last recorded: (the resource's insertion tick, its revision).
    requests: Option<(bevy::ecs::change_detection::Tick, u64)>,
    certificate: Option<Arc<crate::cluster::reader::CertifiedPackage>>,
    scene_present: bool,
    index: destination_scope::BatchCells,
    worlds: HashSet<WorldId>,
    // Keep the dependency scope of stored reports after their request is withdrawn.
    // A later change must revoke those reports as well as pending requests.
    scopes: HashMap<DestinationKey, Arc<[CellKey]>>,
    /// Walks over the pending requests or stored scopes; a settled frame adds none.
    #[cfg(test)]
    walks: usize,
}

fn observe_candidates(
    sources: Res<Sources>,
    materials: Res<ClusterBatchMaterials>,
    scene: Option<Res<ClusterScene>>,
    mut main: ResMut<MainWorld>,
    frame: Option<Res<RenderFrame>>,
    invalidations: Option<ResMut<RenderInvalidations>>,
    mut previous: ResMut<CandidateObserver>,
) {
    let previous = &mut *previous;
    // Record each request's scope when the request set changes: the scope must outlive a
    // withdrawn request whose report is still stored. An unchanged set costs nothing.
    let requests = main
        .get_resource_change_ticks::<DestinationRequests>()
        .zip(main.get_resource::<DestinationRequests>())
        .map(|(ticks, requests)| (ticks.added, requests));
    let stamp = requests
        .as_ref()
        .map(|(added, requests)| (*added, requests.revision()));
    if previous.requests != stamp {
        previous.requests = stamp;
        #[cfg(test)]
        {
            previous.walks += 1;
        }
        for (_, request) in requests.iter().flat_map(|(_, requests)| requests.iter()) {
            previous
                .scopes
                .insert(request.key, Arc::clone(&request.coverage));
        }
    }
    let same_certificate = match (&previous.certificate, &sources.certificate) {
        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
        (None, None) => true,
        _ => false,
    };
    let scope_changed = !same_certificate
        || previous.scene_present != scene.is_some()
        || scene.as_ref().is_some_and(|scene| scene.is_changed());
    if !scope_changed && previous.versions == Some((sources.candidate_version, materials.version)) {
        return;
    }
    #[cfg(test)]
    {
        previous.walks += 1;
    }
    let pending: HashSet<_> = main
        .get_resource::<DestinationRequests>()
        .into_iter()
        .flat_map(|requests| requests.iter())
        .map(|(_, request)| request.key)
        .collect();
    // Stale scopes are dropped only here, where they are read.
    previous.scopes.retain(|key, _| {
        let current = main
            .get_resource::<Generations>()
            .is_none_or(|generations| generations.current(key.world) == key.generation);
        current
            && (pending.contains(key)
                || main
                    .get_resource::<DestinationReports>()
                    .is_some_and(|reports| reports.get(key).is_some()))
    });
    let source_changed = previous
        .versions
        .is_none_or(|(source, _)| source != sources.candidate_version);
    let materials_changed = previous
        .versions
        .is_none_or(|(_, material)| material != materials.version);
    let old_worlds = previous.worlds.clone();
    if scope_changed || previous.versions.is_none() {
        previous.index = scene
            .as_ref()
            .map_or_else(destination_scope::BatchCells::default, |scene| {
                destination_scope::BatchCells::build(scene, sources.certificate.as_deref())
            });
        previous.worlds = sources
            .certificate
            .as_ref()
            .into_iter()
            .flat_map(|certificate| certificate.records())
            .map(|(_, record)| WorldId::exterior(record.worldspace))
            .chain(
                scene
                    .as_ref()
                    .into_iter()
                    .flat_map(|scene| scene.terrain_blocks.iter())
                    .map(|block| WorldId::exterior(block.worldspace_id)),
            )
            .collect();
        previous.certificate = sources.certificate.clone();
        previous.scene_present = scene.is_some();
    }
    previous.worlds.extend(
        sources
            .worlds
            .values()
            .flatten()
            .copied()
            .map(WorldId::exterior),
    );
    let empty_delta = CandidateWorlds::default();
    let changed_batches: HashSet<usize> = if materials_changed {
        materials
            .changed_batches
            .iter()
            .map(|&batch| batch as usize)
            .collect()
    } else {
        HashSet::new()
    };
    let mut effects = destination_scope::Effects::from_delta(
        if source_changed {
            &sources.candidate_worlds
        } else {
            &empty_delta
        },
        &changed_batches,
        &previous.index,
    );
    // A delta belongs to one version only. A missed version, absent material
    // dependency information, or replacement of the whole scope fails closed.
    effects.all |= scope_changed
        || previous.versions.is_none()
        || previous.versions.is_some_and(|(source, material)| {
            (source_changed && source.wrapping_add(1) != sources.candidate_version)
                || (materials_changed && material.wrapping_add(1) != materials.version)
        })
        || (materials_changed && materials.changed_batches.is_empty());
    let mut changed = effects.changed_worlds();
    if effects.all {
        changed.extend(&previous.worlds);
        changed.extend(old_worlds);
    }
    let mut revoke = HashSet::new();
    let mut remove = Vec::new();
    for world in changed {
        let mut observed = false;
        for (key, coverage) in previous.scopes.iter().filter(|(key, _)| key.world == world) {
            observed = true;
            if effects.affects(world, coverage) {
                if pending.contains(key) {
                    revoke.insert(world);
                } else {
                    remove.push(*key);
                }
            }
        }
        // Unknown/manual report scopes retain conservative world invalidation.
        if !observed {
            revoke.insert(world);
        }
    }
    if let Some(mut reports) = main.get_resource_mut::<DestinationReports>() {
        for key in &remove {
            reports.remove(key);
        }
    }
    for key in &remove {
        previous.scopes.remove(key);
    }
    debug!(
        source_version = sources.candidate_version,
        material_version = materials.version,
        global = effects.all,
        affected_cells = effects.cells.len(),
        affected_batches = sources.candidate_worlds.batches.len(),
        material_batches = materials.changed_batches.len(),
        ?revoke,
        stored_reports_removed = remove.len(),
        pending_requests = pending.len(),
        "cluster destination scoped invalidation"
    );
    if let (Some(frame), Some(mut invalidations), Some(mut generations)) =
        (frame, invalidations, main.get_resource_mut::<Generations>())
    {
        for world in revoke {
            generations.bump(world, InvalidationCause::Candidate);
            invalidations.revoke(world, *frame, InvalidationCause::Candidate);
        }
    }
    previous.versions = Some((sources.candidate_version, materials.version));
}

fn selected(view: &ViewGpu, index: u32) -> bool {
    let offset = (index / 32) as usize * 4;
    view.instance_mask_bytes
        .get(offset..offset + 4)
        .is_some_and(|bytes| {
            u32::from_le_bytes(bytes.try_into().unwrap()) & (1 << (index % 32)) != 0
        })
}

struct ViewMembers {
    forward: PhaseMembers,
    depth: PhaseMembers,
    shadow: PhaseMembers,
}
/// Exact resources the later RenderGraph cull encoder reads. Grouping them
/// keeps publish within the system-function parameter limit.
#[derive(bevy::ecs::system::SystemParam)]
struct CullProofResources<'w> {
    frame: Res<'w, ClusterFrame>,
    pipelines: Res<'w, ClusterPipelines>,
    opaque: Res<'w, ViewBinnedRenderPhases<Opaque3d>>,
    shadow: Res<'w, ViewBinnedRenderPhases<Shadow>>,
}

struct DrawProof<'a, 'w> {
    gpu: &'a ClusterGpu,
    cull: &'a CullProofResources<'w>,
    cache: &'a PipelineCache,
    materials: &'a ErasedRenderAssets<PreparedMaterial>,
    allocators: &'a MaterialBindGroupAllocators,
    carriers: &'a HashMap<u32, MainEntity>,
    phases: &'a QueuePhases<'w>,
}
impl DrawProof<'_, '_> {
    fn members(&self, key: &RetainedViewEntity) -> ViewMembers {
        ViewMembers {
            forward: self.phases.forward(key),
            depth: self.phases.prepass(key),
            shadow: self.phases.shadow(key),
        }
    }
    fn batch(
        &self,
        key: &RetainedViewEntity,
        batch: usize,
        shadow: bool,
        prepass: bool,
        members: &ViewMembers,
    ) -> bool {
        let Some(view) = self.gpu.views.get(key) else {
            return false;
        };
        let material = self
            .gpu
            .batches
            .get(batch)
            .and_then(|batch| batch.material)
            .and_then(|id| self.materials.get(id));
        if shadow && material.is_some_and(|material| !material.properties.shadows_enabled) {
            return true;
        }
        // This is a pre-RenderGraph proof: require the exact dependencies and
        // active-view predicate the encoder will use, never a previous epoch.
        if prepared_cluster_cull(self.gpu, &self.cull.frame, &self.cull.pipelines, self.cache)
            .is_none()
            || !cluster_cull_view_eligible(
                view,
                self.cull.opaque.contains_key(key),
                self.cull.shadow.contains_key(key),
            )
        {
            return false;
        }
        if !batch_ready(
            self.gpu,
            view,
            batch,
            shadow,
            prepass,
            (self.cache, self.materials, self.allocators),
        ) {
            return false;
        }
        let Some(group) = table_group(view, batch) else {
            return false;
        };
        if shadow {
            return self
                .carriers
                .get(&group)
                .is_some_and(|main| members.shadow.queued(*main));
        }
        let Some(draw) = view.draws.get(&group) else {
            return false;
        };
        let Some(rep) = view.plan_rep(draw.representative) else {
            return false;
        };
        members.forward.queued(rep.main_entity)
            && (!prepass
                || material.is_some_and(|material| !material.properties.prepass_enabled)
                || members.depth.queued(rep.main_entity))
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn publish(
    gpu: Option<Res<ClusterGpu>>,
    sources: Res<Sources>,
    runtime_state: Option<Res<crate::cluster::runtime_state::RuntimeReferenceState>>,
    requests: Option<Res<ExtractedRequests>>,
    frame: Option<Res<RenderFrame>>,
    reports: Option<ResMut<HandoffReports>>,
    mut published: ResMut<Published>,
    cache: Res<PipelineCache>,
    cull: CullProofResources,
    materials: Res<ErasedRenderAssets<PreparedMaterial>>,
    allocators: Res<MaterialBindGroupAllocators>,
    phases: QueuePhases,
    carriers: Query<(&ClusterBatch, &MainEntity)>,
    views: Query<(Entity, &ExtractedView, Option<&LightEntity>)>,
    local_shadow_lights: LocalShadowLights,
    camera_shadow_views: Query<&ViewLightEntities>,
) {
    let (Some(requests), Some(frame), Some(mut reports)) = (requests, frame, reports) else {
        return;
    };
    let wanted: HashSet<_> = requests
        .iter()
        .filter(|(request, _)| request.handoff == Handoff::Required)
        .map(|(request, _)| request.key)
        .collect();
    for old in published.keys.difference(&wanted) {
        reports.clear(old);
    }
    published.keys = wanted;
    // Withdraw obsolete reports above even when no request needs a new proof.
    if published.keys.is_empty() {
        return;
    }
    let carrier_ids = carriers
        .iter()
        .map(|(marker, main)| (marker.index, *main))
        .collect();
    for (request, generation) in requests
        .iter()
        .filter(|(request, _)| request.handoff == Handoff::Required)
    {
        let mut state = DestinationState::Waiting(Missing::Handoff);
        if request.key.generation != generation {
            state = DestinationState::Stale;
        } else if let Some(gpu) = gpu.as_deref() {
            let same = match (&gpu.certificate, &sources.certificate) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            if same
                && let Some((_, view, _)) = views.iter().find(|(_, view, light)| {
                    light.is_none() && view.retained_view_entity == request.key.view.view
                })
            {
                let key = view.retained_view_entity;
                let world = sources.worlds.get(&key.main_entity);
                if request.key.world.0 & (1 << 63) != 0 {
                    // The certified domains are exterior only. Interior geometry is
                    // intentionally stock, whose actual passes the bridge proves.
                    if world == Some(&None) {
                        state = DestinationState::Ready;
                    }
                } else if let Ok(worldspace) = u32::try_from(request.key.world.0)
                    && world == Some(&Some(worldspace))
                {
                    let proof = DrawProof {
                        gpu,
                        cull: &cull,
                        cache: &cache,
                        materials: &materials,
                        allocators: &allocators,
                        carriers: &carrier_ids,
                        phases: &phases,
                    };
                    if destination_ready(
                        &proof,
                        (&sources, runtime_state.as_deref()),
                        request,
                        worldspace,
                        &views,
                        &local_shadow_lights,
                        &camera_shadow_views,
                    ) {
                        state = DestinationState::Ready;
                    }
                }
            }
        }
        let report = HandoffReport {
            key: request.key,
            evaluated_render_frame: *frame,
            state,
        };
        let state = report.state.clone();
        if reports.publish(report) {
            debug!(?request.key, ?state, "cluster destination handoff changed");
        }
    }
}

/// Cluster-only handoff is legal only for Complete reference coverage. Partial
/// replacement needs every validated stock-only source primitive instantiated;
/// the generic bridge still proves their actual visible queues and passes.
fn replacement_coverage_ready(
    certificate: &super::super::super::reader::CertifiedPackage,
    sources: &Sources,
    reference: u32,
    stock_exists: bool,
) -> bool {
    certificate.reference_complete(reference)
        || (stock_exists
            && certificate
                .unclustered_primitives(reference)
                .is_some_and(|expected| {
                    expected.iter().all(|primitive| {
                        sources
                            .stock_primitives
                            .contains_key(&(reference, *primitive))
                    })
                }))
}

/// Resolve the actual dependent local shadow domain after native light preparation.
fn required_local_shadow_views(
    key: &RetainedViewEntity,
    world: u32,
    sources: &Sources,
    shadow_phases: &ViewBinnedRenderPhases<Shadow>,
    views: &Query<(Entity, &ExtractedView, Option<&LightEntity>)>,
    local_shadow_lights: &LocalShadowLights,
) -> Option<HashSet<Entity>> {
    let Some(Some(dependencies)) = sources.light_dependencies.get(&key.main_entity) else {
        return None;
    };
    let mut required = HashSet::new();
    for dependency in dependencies {
        let Some((_, light, light_views)) = local_shadow_lights
            .iter()
            .find(|(main, _, _)| *main == dependency)
        else {
            // An absent render light is unknown, not an empty native shadow domain.
            return None;
        };
        if !light.shadow_maps_enabled || light_views.is_empty() {
            continue;
        }
        let expected = if light.spot_light_angles.is_some() {
            1
        } else {
            bevy::camera::primitives::CUBE_MAP_FACES.len()
        };
        if light_views.len() != expected {
            return None;
        }
        let mut admitted = HashSet::new();
        for entity in light_views.iter() {
            let Ok((_, view, Some(role))) = views.get(*entity) else {
                return None;
            };
            if !matches!(role, LightEntity::Point { .. } | LightEntity::Spot { .. }) {
                return None;
            }
            // Native preparation can leave old face entities alive when its light/atlas
            // allocation shrinks. The actual shadow pass returns early without this phase.
            if shadow_phases.contains_key(&view.retained_view_entity) {
                admitted.insert(*entity);
            }
        }
        if admitted.is_empty() {
            continue;
        }
        if admitted.len() != expected {
            return None;
        }
        let owner = sources.worlds.get(dependency)?;
        if *owner != Some(world) {
            // An admitted light affects this camera; foreign ownership cannot erase its pass.
            return None;
        }
        required.extend(admitted);
    }
    Some(required)
}

/// Resolve camera-specific native directional shadow allocation after light preparation.
fn required_directional_shadow_views(
    key: &RetainedViewEntity,
    views: &Query<(Entity, &ExtractedView, Option<&LightEntity>)>,
    camera_shadow_views: &Query<&ViewLightEntities>,
    shadow_phases: &ViewBinnedRenderPhases<Shadow>,
) -> Option<Vec<RetainedViewEntity>> {
    let (camera, _, _) = views
        .iter()
        .find(|(_, view, light)| light.is_none() && view.retained_view_entity == *key)?;
    let allocation = camera_shadow_views.get(camera).ok()?;
    let mut required = Vec::with_capacity(allocation.lights.len());
    // Native light selection can leave displaced lights' old view entities alive.
    // The camera's current allocated list is the actual directional shadow pass input.
    for entity in &allocation.lights {
        let (_, view, Some(LightEntity::Directional { .. })) = views.get(*entity).ok()? else {
            return None;
        };
        if view.retained_view_entity.auxiliary_entity != key.main_entity
            || !shadow_phases.contains_key(&view.retained_view_entity)
        {
            return None;
        }
        required.push(view.retained_view_entity);
    }
    Some(required)
}

/// Accepted object obligations in this exact placing world/cell. Material-refused
/// records belong to permanent stock coverage and cannot also require a cluster draw.
fn destination_owned_records<'a>(
    certificate: &'a crate::cluster::reader::CertifiedPackage,
    sources: &'a Sources,
    runtime_state: Option<&'a crate::cluster::runtime_state::RuntimeReferenceState>,
    world: u32,
    grid: IVec2,
) -> impl Iterator<
    Item = (
        &'a crate::cluster::reader::RecordKey,
        &'a crate::cluster::reader::ReferencePrimitive,
    ),
> + 'a {
    certificate
        .records_in_placed_cell(world, grid)
        .filter(move |(record, identity)| {
            !sources.permanently_refused_records.contains(*record)
                && !runtime_state
                    .is_some_and(|state| state.legitimately_disabled(identity.reference))
        })
}

fn destination_ready(
    proof: &DrawProof,
    (sources, runtime_state): (
        &Sources,
        Option<&crate::cluster::runtime_state::RuntimeReferenceState>,
    ),
    request: &DestinationRequest,
    world: u32,
    views: &Query<(Entity, &ExtractedView, Option<&LightEntity>)>,
    local_shadow_lights: &LocalShadowLights,
    camera_shadow_views: &Query<&ViewLightEntities>,
) -> bool {
    let key = request.key.view.view;
    let Some(view) = proof.gpu.views.get(&key) else {
        return false;
    };
    let Some(certificate) = proof.gpu.certificate.as_ref() else {
        return false;
    };
    let prepass = [
        PassFamily::DepthPrepass,
        PassFamily::NormalPrepass,
        PassFamily::MotionPrepass,
    ]
    .into_iter()
    .any(|family| request.passes.contains(family));
    let members = proof.members(&key);
    let local_shadow_views = if request.passes.contains(PassFamily::Shadow) {
        let Some(local) = required_local_shadow_views(
            &key,
            world,
            sources,
            &proof.cull.shadow,
            views,
            local_shadow_lights,
        ) else {
            return false;
        };
        local
    } else {
        HashSet::new()
    };
    let directional = if request.passes.contains(PassFamily::Shadow) {
        let Some(required) =
            required_directional_shadow_views(&key, views, camera_shadow_views, &proof.cull.shadow)
        else {
            return false;
        };
        required
    } else {
        Vec::new()
    };
    let shadows: Vec<_> = views
        .iter()
        .filter(|(entity, view, light)| match light {
            Some(LightEntity::Directional { .. }) => {
                directional.contains(&view.retained_view_entity)
            }
            Some(_) => local_shadow_views.contains(entity),
            None => false,
        })
        .map(|(_, view, _)| {
            (
                view.retained_view_entity,
                proof.members(&view.retained_view_entity),
            )
        })
        .collect();
    for cell in request.coverage.iter() {
        let CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        } = *cell
        else {
            return false;
        };
        if worldspace_id != world {
            return false;
        }
        let grid = IVec2::new(grid_x, grid_y);
        for (record, identity) in
            destination_owned_records(certificate, sources, runtime_state, world, grid)
        {
            if view
                .selection
                .as_ref()
                .is_some_and(|selection| !ViewDomain::new(&selection.layers, false).objects)
            {
                continue;
            }
            let stock_exists = sources.present.contains(&identity.reference)
                && !sources.retired_present.contains(&identity.reference);
            if !replacement_coverage_ready(certificate, sources, identity.reference, stock_exists) {
                return false;
            }
            let Some(index) = proof.gpu.object_indices.get(record).copied() else {
                return false;
            };
            let batch = proof.gpu.placed[index as usize].batch as usize;
            if !selected(view, index) || !proof.batch(&key, batch, false, prepass, &members) {
                return false;
            }
            if request.passes.contains(PassFamily::Shadow) {
                let material = proof
                    .gpu
                    .batches
                    .get(batch)
                    .and_then(|batch| batch.material)
                    .and_then(|id| proof.materials.get(id));
                if material.is_some_and(|material| !material.properties.shadows_enabled) {
                    continue;
                }
                for (shadow_key, shadow_members) in &shadows {
                    let Some(shadow_view) = proof.gpu.views.get(shadow_key) else {
                        return false;
                    };
                    if shadow_view
                        .selection
                        .as_ref()
                        .is_some_and(|selection| !ViewDomain::new(&selection.layers, true).objects)
                    {
                        continue;
                    }
                    if !selected(shadow_view, index)
                        || !proof.batch(shadow_key, batch, true, false, shadow_members)
                    {
                        return false;
                    }
                }
            }
        }
        for (block_index, block) in proof
            .gpu
            .terrain_blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.worldspace_id == world && block.cells.contains(&grid))
        {
            let cell_ready = |key: &RetainedViewEntity,
                              shadow: bool,
                              needs_depth: bool,
                              members: &ViewMembers| {
                let Some(candidate) = proof.gpu.views.get(key) else {
                    return false;
                };
                let Some(inputs) = candidate.terrain_clip_inputs.as_ref().filter(|inputs| {
                    inputs.world == Some(world) && inputs.origin == sources.origin
                }) else {
                    return false;
                };
                terrain_pass_ready(
                    inputs,
                    grid,
                    block_index as u32,
                    shadow,
                    needs_depth,
                    block.batch.is_some_and(|batch| {
                        proof.batch(key, batch as usize, shadow, needs_depth, members)
                    }),
                )
            };
            if !cell_ready(&key, false, prepass, &members) {
                return false;
            }
            if request.passes.contains(PassFamily::Shadow)
                && shadows
                    .iter()
                    .any(|(key, members)| !cell_ready(key, true, false, members))
            {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod partial_coverage_tests {
    use super::*;
    use crate::cluster::reader::producer_fixture_tests::{published, published_for_loader};

    #[test]
    fn partial_cluster_only_waits_for_every_rejected_source_primitive() {
        let directory = published();
        let certificate =
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
        let mut sources = Sources::default();
        let reference = 0x100;
        assert!(!replacement_coverage_ready(
            &certificate,
            &sources,
            reference,
            false
        ));
        assert!(!replacement_coverage_ready(
            &certificate,
            &sources,
            reference,
            true
        ));
        sources
            .stock_primitives
            .insert((reference, 0), Entity::from_raw_u32(1).unwrap().into());
        assert!(!replacement_coverage_ready(
            &certificate,
            &sources,
            reference,
            true
        ));
        sources
            .stock_primitives
            .insert((reference, 1), Entity::from_raw_u32(2).unwrap().into());
        assert!(replacement_coverage_ready(
            &certificate,
            &sources,
            reference,
            true
        ));
        assert!(!replacement_coverage_ready(
            &certificate,
            &sources,
            reference,
            false
        ));
        sources.stock_primitives.remove(&(reference, 1));
        assert!(!replacement_coverage_ready(
            &certificate,
            &sources,
            reference,
            true
        ));
    }

    #[test]
    fn complete_real_reference_needs_no_stock_piece_but_unknown_never_qualifies() {
        let directory = published_for_loader();
        let certificate =
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
        let sources = Sources::default();
        assert!(replacement_coverage_ready(
            &certificate,
            &sources,
            0x100,
            false
        ));
        assert!(!replacement_coverage_ready(
            &certificate,
            &sources,
            0x999,
            false
        ));
        assert!(!replacement_coverage_ready(
            &certificate,
            &sources,
            0x999,
            true
        ));
    }
}

/// Stock coverage is current queued/drawable coverage for this actual pass.
/// Early selection alone never proves a late material or shadow pipeline.
fn terrain_pass_ready(
    inputs: &TerrainClipInputs,
    grid: IVec2,
    block: u32,
    shadow: bool,
    needs_depth: bool,
    cluster_drawable: bool,
) -> bool {
    let forward = (0..4u8).all(|quadrant| inputs.forward_covered.contains(&(grid, quadrant)));
    let depth = (0..4u8).all(|quadrant| inputs.depth_covered.contains(&(grid, quadrant)));
    let stock = if shadow {
        depth
    } else {
        forward && (!needs_depth || depth)
    };
    stock || (inputs.ready_blocks.contains(&block) && cluster_drawable)
}

#[cfg(test)]
mod tests {
    use super::*;

    include!("destination_obligation_tests.rs");

    #[test]
    fn late_clip_cache_rechecks_exact_target_offset_even_with_unchanged_grid_and_far_nonce() {
        let grid_origin = IVec2::new(-4, 8);
        let offset = super::super::render_origin_offset(grid_origin);
        let inputs = TerrainClipInputs {
            world: Some(12),
            origin: grid_origin,
            forward_covered: HashSet::new(),
            depth_covered: HashSet::new(),
            ready_blocks: [0].into_iter().collect(),
            target_offset: offset.to_array().map(f32::to_bits),
        };
        assert!(inputs.target_offset_matches(offset));
        assert!(!inputs.target_offset_matches(offset + Vec3::X));
        assert!(!inputs.target_offset_matches(Vec3::new(offset.x, -0.0, offset.z)));
        assert!(!inputs.target_offset_matches(Vec3::new(offset.x, f32::NAN, offset.z)));
        assert_eq!(inputs.origin, grid_origin);
    }

    #[test]
    fn terrain_selection_does_not_prove_a_missing_shadow_or_depth_draw() {
        let grid = IVec2::new(7, -9);
        let mut inputs = TerrainClipInputs {
            target_offset: [0; 3],
            world: Some(12),
            origin: IVec2::ZERO,
            forward_covered: (0..4).map(|q| (grid, q)).collect(),
            depth_covered: HashSet::new(),
            ready_blocks: [3].into_iter().collect(),
        };
        assert!(terrain_pass_ready(&inputs, grid, 3, false, false, false));
        assert!(!terrain_pass_ready(&inputs, grid, 3, false, true, false));
        assert!(!terrain_pass_ready(&inputs, grid, 3, true, false, false));
        assert!(terrain_pass_ready(&inputs, grid, 3, true, false, true));
        assert!(!terrain_pass_ready(&inputs, grid, 4, true, false, true));
        inputs.depth_covered = (0..4).map(|q| (grid, q)).collect();
        assert!(terrain_pass_ready(&inputs, grid, 4, true, false, false));
        inputs.depth_covered.remove(&(grid, 2));
        inputs.depth_covered.insert((grid + IVec2::X, 2));
        assert!(!terrain_pass_ready(&inputs, grid, 4, true, false, false));
    }
}

#[cfg(test)]
mod terrain_candidate_regressions {
    use super::*;
    use crate::streaming::TerrainPrimitiveIdentity;
    use bevy::ecs::schedule::Schedule;
    use shared::lod::LodTier;

    const WORLD: u32 = 12;
    const GRID: IVec2 = IVec2::new(7, -9);
    fn main(id: u32) -> MainEntity {
        Entity::from_raw_u32(id).unwrap().into()
    }
    fn near_map() -> HashMap<MainEntity, TerrainPrimitiveIdentity> {
        (0..4u8)
            .map(|quadrant| {
                (
                    main(100 + u32::from(quadrant)),
                    TerrainPrimitiveIdentity {
                        worldspace_id: WORLD,
                        grid: GRID,
                        quadrant,
                        tier: None,
                    },
                )
            })
            .collect()
    }

    struct Harness {
        render: World,
        observer: Schedule,
        key: DestinationKey,
    }
    impl Harness {
        fn ready_on_stock() -> Self {
            Self::ready_on_stock_with_map(near_map())
        }
        fn ready_on_stock_with_map(terrain: HashMap<MainEntity, TerrainPrimitiveIdentity>) -> Self {
            let inputs = TerrainClipInputs {
                target_offset: [0; 3],
                world: Some(WORLD),
                origin: IVec2::ZERO,
                forward_covered: (0..4).map(|q| (GRID, q)).collect(),
                depth_covered: (0..4).map(|q| (GRID, q)).collect(),
                ready_blocks: HashSet::new(),
            };
            assert!(
                terrain_pass_ready(&inputs, GRID, 3, false, true, false),
                "Ready is stock-only, with no ready cluster block"
            );
            let mut render = World::new();
            let sources = Sources {
                worlds: HashMap::from([(main(1), Some(WORLD))]),
                terrain_visible: terrain.keys().copied().collect(),
                terrain: terrain.into(),
                ..Default::default()
            };
            render.insert_resource(sources);
            render.init_resource::<ClusterBatchMaterials>();
            render.insert_resource(RenderFrame(1));
            render.init_resource::<RenderInvalidations>();
            let mut main_world = MainWorld::default();
            main_world.init_resource::<Generations>();
            main_world.init_resource::<DestinationReports>();
            render.insert_resource(main_world);
            render.init_resource::<CandidateObserver>();
            let mut observer = Schedule::default();
            observer.add_systems(observe_candidates);
            observer.run(&mut render);
            let generation = render
                .resource::<MainWorld>()
                .resource::<Generations>()
                .current(WorldId::exterior(WORLD));
            let key = DestinationKey {
                view: ViewKey {
                    view: RetainedViewEntity::new(main(1), None, 0),
                    incarnation: 1,
                },
                world: WorldId::exterior(WORLD),
                generation,
            };
            render
                .resource_mut::<MainWorld>()
                .resource_mut::<DestinationReports>()
                .set(DestinationReport {
                    key,
                    evaluated_render_frame: RenderFrame(10),
                    state: DestinationState::Ready,
                });
            render.insert_resource(RenderFrame(11));
            Self {
                render,
                observer,
                key,
            }
        }
        fn ready(&self) -> bool {
            let main = self.render.resource::<MainWorld>();
            main.resource::<DestinationReports>()
                .is_ready(&self.key, main.resource::<Generations>())
        }
        fn apply_terrain(
            &mut self,
            current: HashMap<MainEntity, TerrainPrimitiveIdentity>,
        ) -> TerrainMapChange {
            let visible = self
                .render
                .resource::<Sources>()
                .terrain_visible
                .iter()
                .filter(|entity| current.contains_key(*entity))
                .copied()
                .collect();
            self.apply_inputs(current, visible)
        }
        fn apply_inputs(
            &mut self,
            current: HashMap<MainEntity, TerrainPrimitiveIdentity>,
            visible: HashSet<MainEntity>,
        ) -> TerrainMapChange {
            let change = {
                let mut sources = self.render.resource_mut::<Sources>();
                let change = terrain_map_change(&sources.terrain, &current);
                let (visible_changed, withdrawn) =
                    terrain_visibility_change(&sources.terrain_visible, &visible);
                if change != TerrainMapChange::Unchanged || visible_changed {
                    let delta = {
                        let old = sources.candidate_inputs();
                        let new = CandidateInputs {
                            terrain: &current,
                            terrain_visible: &visible,
                            ..sources.candidate_inputs()
                        };
                        candidate_worlds(&old, &new)
                    };
                    sources.record_change(change == TerrainMapChange::Invalidating || withdrawn);
                    sources.candidate_worlds = delta;
                    sources.terrain = current.into();
                    sources.terrain_visible = visible;
                }
                change
            };
            self.observer.run(&mut self.render);
            change
        }
    }

    #[test]
    fn far_arrival_updates_selection_without_invalidating_prior_stock_ready() {
        let mut h = Harness::ready_on_stock();
        assert!(h.ready());
        let (selection, candidate) = {
            let sources = h.render.resource::<Sources>();
            (sources.version, sources.candidate_version)
        };
        let mut current = near_map();
        current.insert(
            main(999),
            TerrainPrimitiveIdentity {
                worldspace_id: WORLD,
                grid: GRID + IVec2::X,
                quadrant: 0,
                tier: Some(LodTier::Tier8),
            },
        );
        assert_eq!(h.apply_terrain(current), TerrainMapChange::FarAdditionsOnly);
        let sources = h.render.resource::<Sources>();
        assert_eq!(sources.version, selection + 1);
        assert_eq!(sources.candidate_version, candidate);
        assert!(
            h.ready(),
            "old report remains valid without any newly evaluated report"
        );
        assert_eq!(
            h.render
                .resource::<MainWorld>()
                .resource::<DestinationReports>()
                .get(&h.key)
                .unwrap()
                .evaluated_render_frame,
            RenderFrame(10)
        );
    }

    #[test]
    fn terrain_withdrawal_reassignment_and_near_arrival_revoke_before_a_new_report() {
        for mutation in ["withdraw", "reassign", "near_addition", "far_withdraw"] {
            let mut h = Harness::ready_on_stock();
            let mut current = near_map();
            match mutation {
                "withdraw" => {
                    current.remove(&main(100));
                }
                "reassign" => {
                    current.get_mut(&main(100)).unwrap().grid += IVec2::Y;
                }
                "near_addition" => {
                    current.insert(
                        main(998),
                        TerrainPrimitiveIdentity {
                            worldspace_id: WORLD,
                            grid: GRID + IVec2::X,
                            quadrant: 0,
                            tier: None,
                        },
                    );
                }
                "far_withdraw" => {
                    let far = TerrainPrimitiveIdentity {
                        worldspace_id: WORLD,
                        grid: GRID + IVec2::X,
                        quadrant: 0,
                        tier: Some(LodTier::Tier16),
                    };
                    let mut arrival = current.clone();
                    arrival.insert(main(999), far);
                    assert_eq!(h.apply_terrain(arrival), TerrainMapChange::FarAdditionsOnly);
                }
                _ => unreachable!(),
            }
            assert!(h.ready());
            assert_eq!(
                h.apply_terrain(current),
                TerrainMapChange::Invalidating,
                "{mutation}"
            );
            assert!(!h.ready(), "old Ready must fail immediately for {mutation}");
            let main = h.render.resource::<MainWorld>();
            assert_eq!(
                main.resource::<DestinationReports>()
                    .get(&h.key)
                    .unwrap()
                    .state,
                DestinationState::Ready,
                "the stored Ready is deliberately not overwritten by a newer evaluation"
            );
        }
    }

    #[test]
    fn other_world_candidate_churn_preserves_ready_and_delta_is_consumed_once() {
        let mut h = Harness::ready_on_stock();
        let foreign_world = WORLD + 1;
        h.render
            .resource_mut::<Sources>()
            .worlds
            .insert(main(2), Some(foreign_world));
        h.render.resource_mut::<Sources>().record_change(true);
        h.render.resource_mut::<Sources>().candidate_worlds = CandidateWorlds {
            all: false,
            worlds: HashSet::from([foreign_world]),
            ..default()
        };
        h.observer.run(&mut h.render);
        assert!(
            h.ready(),
            "a source-world change cannot stale a different destination"
        );
        let generation = h
            .render
            .resource::<MainWorld>()
            .resource::<Generations>()
            .current(WorldId::exterior(foreign_world));
        h.observer.run(&mut h.render);
        assert_eq!(
            h.render
                .resource::<MainWorld>()
                .resource::<Generations>()
                .current(WorldId::exterior(foreign_world)),
            generation
        );
        assert!(h.ready());
        // A later global material change still revokes it without a new source version.
        h.render.resource_mut::<ClusterBatchMaterials>().version += 1;
        h.observer.run(&mut h.render);
        assert!(!h.ready());
    }

    fn cover(grid: IVec2) -> CellKey {
        CellKey::Exterior {
            worldspace_id: WORLD,
            grid_x: grid.x,
            grid_y: grid.y,
        }
    }

    /// A pending request for the harness's key, as the crossing code holds while it waits.
    fn request_coverage(h: &mut Harness, coverage: Vec<CellKey>) -> RequestId {
        let mut requests = DestinationRequests::default();
        let id = requests.request(DestinationRequest {
            key: h.key,
            passes: PassFamilies::NONE,
            coverage: Arc::from(coverage),
            handoff: Handoff::NotRequired,
        });
        h.render
            .resource_mut::<MainWorld>()
            .insert_resource(requests);
        // The observer records a request's coverage the first time it sees it.
        h.observer.run(&mut h.render);
        id
    }

    fn near_withdrawal() -> HashMap<MainEntity, TerrainPrimitiveIdentity> {
        let mut current = near_map();
        current.remove(&main(100));
        current
    }

    fn generation(h: &Harness) -> Generation {
        h.render
            .resource::<MainWorld>()
            .resource::<Generations>()
            .current(h.key.world)
    }

    #[test]
    fn same_world_change_outside_a_pending_destinations_coverage_does_not_bump_it() {
        let mut h = Harness::ready_on_stock();
        request_coverage(&mut h, vec![cover(GRID + IVec2::X)]);
        let before = generation(&h);
        assert_eq!(
            h.apply_terrain(near_withdrawal()),
            TerrainMapChange::Invalidating
        );
        assert_eq!(
            generation(&h),
            before,
            "an unrelated cell must not restart it"
        );
        assert!(h.ready());
    }

    #[test]
    fn same_world_change_inside_a_pending_destinations_coverage_bumps_it() {
        let mut h = Harness::ready_on_stock();
        request_coverage(&mut h, vec![cover(GRID + IVec2::X), cover(GRID)]);
        let before = generation(&h);
        h.apply_terrain(near_withdrawal());
        assert_ne!(generation(&h), before);
        assert!(!h.ready());
    }

    #[test]
    fn settled_frames_walk_no_requests_or_scopes() {
        let mut h = Harness::ready_on_stock();
        let id = request_coverage(&mut h, vec![cover(GRID)]);
        let walks = |h: &Harness| h.render.resource::<CandidateObserver>().walks;
        let settled = walks(&h);
        for _ in 0..8 {
            // A waiting consumer rebases every frame; an unchanged generation is no change.
            let mut main = h.render.resource_mut::<MainWorld>();
            let generations = main.remove_resource::<Generations>().unwrap();
            main.resource_mut::<DestinationRequests>()
                .rebase(id, &generations);
            main.insert_resource(generations);
            h.observer.run(&mut h.render);
        }
        assert_eq!(
            walks(&h),
            settled,
            "a settled frame walked requests or scopes"
        );
        h.render
            .resource_mut::<MainWorld>()
            .resource_mut::<DestinationRequests>()
            .withdraw(id);
        h.observer.run(&mut h.render);
        assert_eq!(walks(&h), settled + 1, "a withdrawal is one change");
        h.apply_terrain(near_withdrawal());
        assert_eq!(walks(&h), settled + 2, "a candidate change is one change");
    }

    #[test]
    fn stored_report_keeps_its_scope_after_the_request_is_withdrawn() {
        let mut h = Harness::ready_on_stock();
        let id = request_coverage(&mut h, vec![cover(GRID + IVec2::X)]);
        h.render
            .resource_mut::<MainWorld>()
            .resource_mut::<DestinationRequests>()
            .withdraw(id);
        h.observer.run(&mut h.render);
        h.apply_terrain(near_withdrawal());
        assert!(h.ready(), "its coverage never included the changed cell");
        let mut moved = near_withdrawal();
        moved.get_mut(&main(101)).unwrap().grid = GRID + IVec2::X;
        h.apply_terrain(moved);
        assert!(
            !h.ready(),
            "a change that reaches its coverage revokes the stored report"
        );
    }

    #[test]
    fn candidate_and_material_paths_still_revoke_existing_ready_reports() {
        let mut h = Harness::ready_on_stock();
        h.render.resource_mut::<Sources>().record_change(true);
        h.observer.run(&mut h.render);
        assert!(!h.ready());
        let mut h = Harness::ready_on_stock();
        h.render.resource_mut::<ClusterBatchMaterials>().version += 1;
        h.observer.run(&mut h.render);
        assert!(!h.ready());
    }

    #[test]
    fn unchanged_terrain_does_not_advance_either_counter() {
        let mut h = Harness::ready_on_stock();
        assert_eq!(h.apply_terrain(near_map()), TerrainMapChange::Unchanged);
        let sources = h.render.resource::<Sources>();
        assert_eq!((sources.version, sources.candidate_version), (0, 0));
        assert!(h.ready());
    }
    fn far_stock_map() -> HashMap<MainEntity, TerrainPrimitiveIdentity> {
        near_map()
            .into_iter()
            .map(|(entity, mut identity)| {
                identity.tier = Some(LodTier::Tier16);
                (entity, identity)
            })
            .collect()
    }

    #[test]
    fn finer_far_arrival_hiding_the_only_old_stock_revokes_stored_ready() {
        let old = far_stock_map();
        let mut h = Harness::ready_on_stock_with_map(old.clone());
        assert!(h.ready());
        let mut current = old;
        let mut new_visible = HashSet::new();
        for quadrant in 0..4u8 {
            let entity = main(200 + u32::from(quadrant));
            current.insert(
                entity,
                TerrainPrimitiveIdentity {
                    worldspace_id: WORLD,
                    grid: GRID,
                    quadrant,
                    tier: Some(LodTier::Tier8),
                },
            );
            new_visible.insert(entity);
        }
        // Identity map is a strict FAR-only superset, but visibility switches
        // from drawable old stock to new stock whose required passes are absent.
        assert_eq!(
            terrain_map_change(&h.render.resource::<Sources>().terrain, &current),
            TerrainMapChange::FarAdditionsOnly
        );
        let not_prepared = TerrainClipInputs {
            target_offset: [0; 3],
            world: Some(WORLD),
            origin: IVec2::ZERO,
            forward_covered: HashSet::new(),
            depth_covered: HashSet::new(),
            ready_blocks: HashSet::new(),
        };
        assert!(!terrain_pass_ready(
            &not_prepared,
            GRID,
            3,
            false,
            true,
            false
        ));
        assert_eq!(
            h.apply_inputs(current, new_visible),
            TerrainMapChange::FarAdditionsOnly
        );
        assert!(
            !h.ready(),
            "old stock-only Ready must be rejected before the new Waiting is mirrored"
        );
        assert_eq!(
            h.render
                .resource::<MainWorld>()
                .resource::<DestinationReports>()
                .get(&h.key)
                .unwrap()
                .state,
            DestinationState::Ready,
            "the test intentionally preserves the older report rather than replacing it"
        );
    }

    #[test]
    fn unchanged_identities_with_raw_visible_member_lost_revoke_stored_ready() {
        let current = far_stock_map();
        let mut h = Harness::ready_on_stock_with_map(current.clone());
        let mut visible: HashSet<_> = current.keys().copied().collect();
        visible.remove(&main(100));
        assert_eq!(
            h.apply_inputs(current, visible),
            TerrainMapChange::Unchanged
        );
        assert!(!h.ready());
        let sources = h.render.resource::<Sources>();
        assert_eq!((sources.version, sources.candidate_version), (1, 1));
    }

    #[test]
    fn far_and_visible_additions_preserving_old_visible_sources_do_not_revoke() {
        let old = far_stock_map();
        let mut h = Harness::ready_on_stock_with_map(old.clone());
        let old_visible: HashSet<_> = old.keys().copied().collect();
        let mut current = old;
        current.insert(
            main(999),
            TerrainPrimitiveIdentity {
                worldspace_id: WORLD,
                grid: GRID + IVec2::X,
                quadrant: 0,
                tier: Some(LodTier::Tier8),
            },
        );
        assert_eq!(
            h.apply_inputs(current.clone(), old_visible.clone()),
            TerrainMapChange::FarAdditionsOnly
        );
        assert!(h.ready());
        let mut all_visible = old_visible;
        all_visible.insert(main(999));
        assert_eq!(
            h.apply_inputs(current, all_visible),
            TerrainMapChange::Unchanged
        );
        assert!(h.ready());
        let sources = h.render.resource::<Sources>();
        assert_eq!((sources.version, sources.candidate_version), (2, 0));
    }

    #[test]
    fn real_destination_owner_installs_required_publication_in_the_render_graph() {
        let mut app = App::new();
        let mut render = bevy::app::SubApp::new();
        render.add_schedule(Render::base_schedule());
        render
            .world_mut()
            .init_resource::<bevy::render::MainWorld>();
        app.insert_sub_app(RenderApp, render);
        app.add_plugins(crate::swap_bridge::systems::DestinationReadinessPlugin);
        install(&mut app);
        let published =
            app.sub_app_mut(RenderApp)
                .world_mut()
                .schedule_scope(Render, |world, schedule| {
                    schedule
                        .initialize(world)
                        .expect("real publication/evaluation ordering must be acyclic");
                    schedule
                        .graph()
                        .systems_in_set(bevy::ecs::schedule::SystemSet::intern(
                            &crate::swap_bridge::systems::DestinationSystems::PublishHandoff,
                        ))
                        .map_or(0, |systems| systems.len())
                });
        assert_eq!(
            published, 1,
            "Required destinations need their actual cluster publication owner, not only candidate invalidation"
        );
        assert!(app.world().contains_resource::<RequiredHandoff>());
    }
}

#[cfg(test)]
mod native_shadow_domain_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn local_light(spot: bool) -> ExtractedPointLight {
        ExtractedPointLight {
            color: LinearRgba::WHITE,
            intensity: 1.0,
            range: 10.0,
            radius: 0.0,
            transform: GlobalTransform::IDENTITY,
            shadow_maps_enabled: true,
            contact_shadows_enabled: false,
            shadow_depth_bias: 0.0,
            shadow_normal_bias: 0.0,
            shadow_map_near_z: 0.1,
            spot_light_angles: spot.then_some((0.2, 0.4)),
            volumetric: false,
            soft_shadows_enabled: false,
            affects_lightmapped_mesh_diffuse: true,
        }
    }

    fn proof(
        metadata: bool,
        spot: bool,
        allocated: usize,
        arrived: usize,
    ) -> Option<HashSet<Entity>> {
        proof_with_admission(metadata, spot, allocated, arrived, allocated)
    }

    pub(super) fn proof_with_admission(
        metadata: bool,
        spot: bool,
        allocated: usize,
        arrived: usize,
        live_phases: usize,
    ) -> Option<HashSet<Entity>> {
        let mut world = World::new();
        let mut phases = ViewBinnedRenderPhases::<Shadow>::default();
        let camera: MainEntity = world.spawn_empty().id().into();
        let light: MainEntity = world.spawn_empty().id().into();
        let mut sources = Sources::default();
        sources.worlds.insert(light, Some(42));
        sources
            .light_dependencies
            .insert(camera, Some(HashSet::from([light])));
        let mut light_views = PointAndSpotLightViewEntities::default();
        for index in 0..allocated {
            if index < live_phases {
                phases.prepare_for_new_frame(
                    RetainedViewEntity::new(light, None, index as u32),
                    bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode::None,
                );
            }
            let mut entity = world.spawn_empty();
            if index < arrived {
                entity.insert((
                    ExtractedView {
                        retained_view_entity: RetainedViewEntity::new(light, None, index as u32),
                        clip_from_view: Mat4::IDENTITY,
                        world_from_view: GlobalTransform::IDENTITY,
                        clip_from_world: None,
                        target_format: bevy::render::render_resource::TextureFormat::Rgba16Float,
                        viewport: UVec4::new(0, 0, 64, 64),
                        color_grading: default(),
                        invert_culling: false,
                    },
                    if spot {
                        LightEntity::Spot {
                            light_entity: light.id(),
                        }
                    } else {
                        LightEntity::Point {
                            light_entity: light.id(),
                            face_index: index,
                        }
                    },
                ));
            }
            light_views.push(entity.id());
        }
        if metadata {
            world.spawn((light, local_light(spot), light_views));
        }
        world
            .run_system_once(
                move |views: Query<(Entity, &ExtractedView, Option<&LightEntity>)>,
                      lights: LocalShadowLights| {
                    required_local_shadow_views(
                        &RetainedViewEntity::new(camera, None, 0),
                        42,
                        &sources,
                        &phases,
                        &views,
                        &lights,
                    )
                },
            )
            .unwrap()
    }

    #[test]
    fn native_unadmitted_local_shadow_lights_have_no_required_pass() {
        // Native preparation leaves an empty allocated list when light/atlas limits exclude
        // a configured shadow caster. Its ExtractedPointLight flag remains enabled.
        for spot in [false, true] {
            assert_eq!(proof(true, spot, 0, 0), Some(HashSet::new()));
        }
    }

    #[test]
    fn unknown_local_shadow_metadata_and_incomplete_admitted_views_remain_pending() {
        for spot in [false, true] {
            assert_eq!(proof(false, spot, 0, 0), None);
            let expected = if spot {
                1
            } else {
                bevy::camera::primitives::CUBE_MAP_FACES.len()
            };
            assert_eq!(proof(true, spot, expected, expected - 1), None);
            let complete =
                proof(true, spot, expected, expected).expect("all admitted native views arrived");
            assert_eq!(complete.len(), expected);
        }
        assert_eq!(
            proof(true, false, 1, 1),
            None,
            "a partial cube is not an unadmitted light"
        );
    }
}

#[cfg(test)]
mod native_shadow_phase_tests {
    use super::native_shadow_domain_tests::proof_with_admission;

    #[test]
    fn native_local_shadow_budget_shrink_does_not_require_stale_face_views() {
        for spot in [false, true] {
            let count = if spot {
                1
            } else {
                bevy::camera::primitives::CUBE_MAP_FACES.len()
            };
            assert_eq!(
                proof_with_admission(true, spot, count, count, 0),
                Some(Default::default()),
                "native preparation removed the phases even if old face entities remain"
            );
        }
    }

    #[test]
    fn partial_native_shadow_phase_admission_does_not_prove_a_complete_cube() {
        let count = bevy::camera::primitives::CUBE_MAP_FACES.len();
        assert_eq!(
            proof_with_admission(true, false, count, count, count - 1),
            None
        );
    }
}

#[cfg(test)]
mod native_directional_admission_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn extracted(key: RetainedViewEntity) -> ExtractedView {
        ExtractedView {
            retained_view_entity: key,
            clip_from_view: Mat4::IDENTITY,
            world_from_view: GlobalTransform::IDENTITY,
            clip_from_world: None,
            target_format: bevy::render::render_resource::TextureFormat::Rgba16Float,
            viewport: UVec4::new(0, 0, 64, 64),
            color_grading: default(),
            invert_culling: false,
        }
    }

    fn evidence(
        allocation: Option<bool>,
        arrived: bool,
        phase: bool,
    ) -> Option<Vec<RetainedViewEntity>> {
        let mut world = World::new();
        let camera = world.spawn_empty().id();
        let light = world.spawn_empty().id();
        let camera_key = RetainedViewEntity::new(camera.into(), None, 0);
        let light_key = RetainedViewEntity::new(light.into(), Some(camera.into()), 0);
        world.entity_mut(camera).insert(extracted(camera_key));
        let mut shadow = world.spawn_empty();
        if arrived {
            shadow.insert((
                extracted(light_key),
                LightEntity::Directional {
                    light_entity: light,
                    cascade_index: 0,
                },
            ));
        }
        let shadow = shadow.id();
        if let Some(allocated) = allocation {
            world.entity_mut(camera).insert(ViewLightEntities {
                lights: if allocated { vec![shadow] } else { Vec::new() },
            });
        }
        let mut phases = ViewBinnedRenderPhases::<Shadow>::default();
        if phase {
            phases.prepare_for_new_frame(
                light_key,
                bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode::None,
            );
        }
        world
            .run_system_once(
                move |views: Query<(Entity, &ExtractedView, Option<&LightEntity>)>,
                      allocation: Query<&ViewLightEntities>| {
                    required_directional_shadow_views(&camera_key, &views, &allocation, &phases)
                },
            )
            .unwrap()
    }

    #[test]
    fn native_directional_displacement_does_not_require_surviving_old_cascades() {
        assert_eq!(evidence(Some(false), true, false), Some(Vec::new()));
    }

    #[test]
    fn native_directional_unknown_allocation_and_missing_admitted_view_remain_pending() {
        assert_eq!(evidence(None, false, false), None);
        assert_eq!(evidence(Some(true), false, true), None);
        assert_eq!(evidence(Some(true), true, false), None);
    }

    #[test]
    fn native_directional_complete_allocated_view_is_the_required_domain() {
        let complete =
            evidence(Some(true), true, true).expect("current allocated view and phase arrived");
        assert_eq!(complete.len(), 1);
        assert_eq!(evidence(Some(false), false, false), Some(Vec::new()));
    }
}
