//! The systems of the destination readiness protocol ([`super::destination`]): the render world
//! evaluates every pending request after its queue step, the next extract copies the result into the
//! main world, and a main-world system combines it with residency and assets into a
//! [`DestinationReport`].
//!
//! Frame order, on the render clock ([`RenderFrame`]): extract N increments the clock, copies the
//! pending requests into the render world and copies the evaluation of frame N-1 back; the render
//! schedule of frame N evaluates (after the queue systems have specialized and queued and the material
//! bind groups were prepared, before the passes draw);
//! extract N+1 reports it. So a report describes the previously evaluated frame and is labelled so.
//!
//! Cost: nothing runs for facts 3 and 4 while no request is pending. With a request pending the
//! render evaluation visits what the requested view will draw (its visible mesh entities, and the
//! entities of its dependent shadow views when shadows are required): O(visible), for the few
//! frames a consumer waits.

use super::{
    assets::LandingAssets,
    destination::{
        DestinationFacts, DestinationKey, DestinationReport, DestinationReports,
        DestinationRequest, DestinationRequests, Generation, Generations, PassFamily, RenderFrame,
        RenderInvalidations, RenderProof, RequestId, Residency, destination_state,
    },
};
use crate::streaming::{PendingUnder, StreamingWorld};
use bevy::{
    camera::visibility::RenderLayers,
    core_pipeline::{
        core_3d::{AlphaMask3d, Opaque3d, Transparent3d},
        prepass::{AlphaMask3dPrepass, Opaque3dPrepass},
    },
    ecs::system::SystemParam,
    pbr::{
        LightEntity, MaterialBindGroupAllocators, PreparedMaterial, RenderMaterialInstances,
        RenderMeshInstanceFlags, RenderMeshInstances, Shadow, SpecializedMaterialPipelineCache,
        SpecializedPrepassMaterialPipelineCache, SpecializedShadowMaterialPipelineCache,
        Transmissive3d, ViewLightEntities,
    },
    prelude::*,
    render::{
        ExtractSchedule, MainWorld, Render, RenderApp, RenderSystems,
        erased_render_asset::ErasedRenderAssets,
        mesh::{RenderMesh, allocator::MeshAllocator},
        render_asset::RenderAssets,
        render_phase::{
            BinnedPhaseItem, BinnedRenderPhase, SortedPhaseItem, SortedRenderPhase,
            ViewBinnedRenderPhases, ViewSortedRenderPhases,
        },
        render_resource::{CachedPipelineState, CachedRenderPipelineId, PipelineCache},
        sync_world::MainEntity,
        view::{
            ExtractedView, RenderShadowMapVisibleEntities, RenderVisibleEntities,
            RenderVisibleEntitiesClass, RetainedViewEntity,
        },
    },
};
use std::collections::{HashMap, HashSet};

/// What the render world holds for one visible entity, as plain facts: the proof is a pure function
/// of these, tested without a GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntityFacts {
    /// The mesh's vertex and index buffers exist in the allocator.
    pub mesh_allocated: bool,
    /// The mesh's render form is prepared.
    pub render_mesh: bool,
    /// The material is drawable: prepared, with a slab and a bind group the draw command can set
    /// (which needs every image it binds).
    pub material_prepared: bool,
    /// Per pass family (by [`PassFamily::index`]): `None` when the entity needs no pipeline in that
    /// family (the family is not required, or the material has no such pass); `Some(true)` when a
    /// queued pipeline resolves; `Some(false)` when it is missing.
    pub passes: [Option<bool>; 5],
}

/// Folds entity facts into one [`RenderProof`].
#[derive(Default, Debug)]
pub struct ProofAccumulator {
    proof: RenderProof,
    pass_missing: [bool; 5],
}

impl ProofAccumulator {
    /// Counts one entity.
    pub fn add(&mut self, facts: &EntityFacts) {
        self.proof.checked += 1;
        if !facts.mesh_allocated || !facts.render_mesh {
            self.proof.geometry_missing += 1;
        }
        if !facts.material_prepared {
            self.proof.material_missing += 1;
        }
        for (missing, pass) in self.pass_missing.iter_mut().zip(facts.passes) {
            *missing |= pass == Some(false);
        }
    }

    /// Marks a required family as missing without an entity to blame: the thing that would carry its
    /// pipelines does not exist yet.
    pub fn mark_pass_missing(&mut self, family: PassFamily) {
        self.pass_missing[family.index()] = true;
    }

    /// The proof, naming the first missing pass family in family order.
    pub fn finish(mut self) -> RenderProof {
        self.proof.pass_missing = PassFamily::ALL
            .into_iter()
            .find(|family| self.pass_missing[family.index()]);
        self.proof
    }
}

/// What the render world proved for one request in one render frame.
#[derive(Clone, Debug)]
pub struct EvaluatedProof {
    pub key: DestinationKey,
    /// The render frame whose queue step this describes.
    pub frame: RenderFrame,
    /// The world's generation the render world recorded when it extracted the request.
    pub extracted_generation: Generation,
    pub proof: RenderProof,
}

/// Main-world consumers run after this set to read the newly combined render report.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DestinationReportsUpdated;

/// Render evaluation runs after material preparation and before the passes draw.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DestinationSystems {
    /// Evaluation of the pending requests for the current render frame.
    Evaluate,
}

/// Main world: what the render world copied back at the last extract.
#[derive(Resource, Default, Debug)]
pub struct RenderResults {
    /// Whether a renderer exists at all. Without one (a headless run, a test) there is nothing to
    /// prove drawable, and facts 3 and 4 hold vacuously, as a settle's drawn count does.
    pub renderer: bool,
    /// The render frame of the last extract.
    pub frame: RenderFrame,
    proofs: HashMap<RequestId, EvaluatedProof>,
}

#[cfg(test)]
impl RenderResults {
    pub(crate) fn insert_for_test(&mut self, id: RequestId, proof: EvaluatedProof) {
        self.proofs.insert(id, proof);
    }
}

struct ExtractedRequest {
    id: RequestId,
    request: DestinationRequest,
    extracted_generation: Generation,
}

/// Render world: the requests as of this frame's extract.
#[derive(Resource, Default)]
pub(crate) struct ExtractedRequests {
    list: Vec<ExtractedRequest>,
}

/// Render world: the newest evaluation of each pending request.
#[derive(Resource, Default)]
pub(crate) struct Evaluations {
    map: HashMap<RequestId, EvaluatedProof>,
}

/// Increments the render clock, copies the pending requests into the render world and the previous
/// evaluation and invalidation records of the requested keys back into the main world.
pub(crate) fn extract_destination_protocol(
    mut main_world: ResMut<MainWorld>,
    mut frame: ResMut<RenderFrame>,
    mut extracted: ResMut<ExtractedRequests>,
    mut evaluations: ResMut<Evaluations>,
    invalidations: Res<RenderInvalidations>,
) {
    frame.0 += 1;
    extracted.list.clear();
    {
        let requests = main_world.resource::<DestinationRequests>();
        let generations = main_world.resource::<Generations>();
        for (id, request) in requests.iter() {
            extracted.list.push(ExtractedRequest {
                id,
                request: request.clone(),
                extracted_generation: generations.current(request.key.world),
            });
        }
    }
    evaluations
        .map
        .retain(|id, _| extracted.list.iter().any(|request| request.id == *id));
    // The render world's invalidation record is mirrored whenever it moved, whether or not a request
    // is pending: a ready report stored earlier must see a late revocation.
    if invalidations.is_changed() {
        main_world
            .resource_mut::<DestinationReports>()
            .mirror_invalidations(&invalidations);
    }
    let mut results = main_world.resource_mut::<RenderResults>();
    results.frame = *frame;
    results.proofs.clear();
    for request in extracted.list.iter() {
        if let Some(proof) = evaluations.map.get(&request.id) {
            results.proofs.insert(request.id, proof.clone());
        }
    }
}

#[cfg(test)]
thread_local! {
    // Per-thread so independent test cases cannot disturb the work assertion.
    static FORWARD_PHASE_WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// The entities one render phase of one view holds right now: the batchable and unbatchable bins,
/// the multidrawable batch sets (through the read-only accessor the vendored `bevy_render` adds) and
/// the sorted items. An entity absent from all of them is not queued, whatever else the phase holds.
#[derive(Default)]
pub(crate) struct PhaseMembers {
    entities: HashSet<MainEntity>,
}

impl PhaseMembers {
    fn extend(&mut self, entities: impl IntoIterator<Item = MainEntity>) {
        #[cfg(test)]
        let entities = entities.into_iter().inspect(|_| {
            FORWARD_PHASE_WORK.with(|work| {
                let (builds, visits) = work.get();
                work.set((builds, visits + 1));
            });
        });
        self.entities.extend(entities);
    }

    fn of_binned<BPI: BinnedPhaseItem>(phase: Option<&BinnedRenderPhase<BPI>>) -> Self {
        let mut members = Self::default();
        let Some(phase) = phase else {
            return members;
        };
        #[cfg(test)]
        crate::work_counters::record("phase_membership/binned_scan");
        for bin in phase.batchable_meshes.values() {
            members.extend(bin.entities().keys().copied());
        }
        for bin in phase.unbatchable_meshes.values() {
            members.extend(bin.entities.keys().copied());
        }
        for set in phase.multidrawable_meshes.values() {
            members.extend(set.entities());
        }
        for bin in phase.non_mesh_items.values() {
            members.extend(bin.entities.keys().copied());
        }
        members
    }

    fn of_sorted<SPI: SortedPhaseItem>(phase: Option<&SortedRenderPhase<SPI>>) -> Self {
        let mut members = Self::default();
        if let Some(phase) = phase {
            #[cfg(test)]
            crate::work_counters::record("phase_membership/sorted_scan");
            members.extend(phase.items.keys().map(|(_, main)| *main));
        }
        members
    }

    fn merge(&mut self, other: Self) {
        self.entities.extend(other.entities);
    }

    /// Whether `main` is queued in one of the phases.
    pub(crate) fn queued(&self, main: MainEntity) -> bool {
        self.entities.contains(&main)
    }
}

/// The render phases the proof reads, optional because a pass the app never added has none.
#[derive(SystemParam)]
pub(crate) struct QueuePhases<'w> {
    opaque: Option<Res<'w, ViewBinnedRenderPhases<Opaque3d>>>,
    alpha_mask: Option<Res<'w, ViewBinnedRenderPhases<AlphaMask3d>>>,
    transmissive: Option<Res<'w, ViewSortedRenderPhases<Transmissive3d>>>,
    transparent: Option<Res<'w, ViewSortedRenderPhases<Transparent3d>>>,
    opaque_prepass: Option<Res<'w, ViewBinnedRenderPhases<Opaque3dPrepass>>>,
    alpha_mask_prepass: Option<Res<'w, ViewBinnedRenderPhases<AlphaMask3dPrepass>>>,
    shadow: Option<Res<'w, ViewBinnedRenderPhases<Shadow>>>,
}

impl QueuePhases<'_> {
    /// Whether the current forward queue contains this main-world entity.
    pub(crate) fn contains_forward(&self, view: &RetainedViewEntity, main: MainEntity) -> bool {
        self.opaque
            .as_ref()
            .and_then(|phases| phases.get(view))
            .is_some_and(|phase| phase.contains_main_entity(main))
            || self
                .alpha_mask
                .as_ref()
                .and_then(|phases| phases.get(view))
                .is_some_and(|phase| phase.contains_main_entity(main))
            || self
                .transmissive
                .as_ref()
                .and_then(|phases| phases.get(view))
                .is_some_and(|phase| phase.contains_main_entity(main))
            || self
                .transparent
                .as_ref()
                .and_then(|phases| phases.get(view))
                .is_some_and(|phase| phase.contains_main_entity(main))
    }

    /// Whether the current prepass queue contains this main-world entity.
    pub(crate) fn contains_prepass(&self, view: &RetainedViewEntity, main: MainEntity) -> bool {
        self.opaque_prepass
            .as_ref()
            .and_then(|phases| phases.get(view))
            .is_some_and(|phase| phase.contains_main_entity(main))
            || self
                .alpha_mask_prepass
                .as_ref()
                .and_then(|phases| phases.get(view))
                .is_some_and(|phase| phase.contains_main_entity(main))
    }

    /// Whether the current shadow queue contains this main-world entity.
    pub(crate) fn contains_shadow(&self, view: &RetainedViewEntity, main: MainEntity) -> bool {
        self.shadow
            .as_ref()
            .and_then(|phases| phases.get(view))
            .is_some_and(|phase| phase.contains_main_entity(main))
    }

    /// What the view's main (forward) phases hold.
    pub(crate) fn forward(&self, view: &RetainedViewEntity) -> PhaseMembers {
        let mut members = PhaseMembers::of_binned(self.opaque.as_ref().and_then(|p| p.get(view)));
        members.merge(PhaseMembers::of_binned(
            self.alpha_mask.as_ref().and_then(|p| p.get(view)),
        ));
        members.merge(PhaseMembers::of_sorted(
            self.transmissive.as_ref().and_then(|p| p.get(view)),
        ));
        members.merge(PhaseMembers::of_sorted(
            self.transparent.as_ref().and_then(|p| p.get(view)),
        ));
        members
    }

    /// What the view's prepass phases hold.
    pub(crate) fn prepass(&self, view: &RetainedViewEntity) -> PhaseMembers {
        let mut members =
            PhaseMembers::of_binned(self.opaque_prepass.as_ref().and_then(|p| p.get(view)));
        members.merge(PhaseMembers::of_binned(
            self.alpha_mask_prepass.as_ref().and_then(|p| p.get(view)),
        ));
        members
    }

    /// What one shadow subview's phase holds.
    pub(crate) fn shadow(&self, view: &RetainedViewEntity) -> PhaseMembers {
        PhaseMembers::of_binned(self.shadow.as_ref().and_then(|p| p.get(view)))
    }
}

fn pipeline_ok(pipelines: &PipelineCache, id: CachedRenderPipelineId) -> bool {
    matches!(
        pipelines.get_render_pipeline_state(id),
        CachedPipelineState::Ok(_)
    )
}

/// The visible mesh entities of one view or subview: the CPU-culled list and the GPU-culled table.
fn visible_meshes(visible: &RenderVisibleEntities) -> impl Iterator<Item = MainEntity> + '_ {
    let class: Option<&RenderVisibleEntitiesClass> = visible.get::<Mesh3d>();
    class.into_iter().flat_map(|class| {
        class
            .entities_cpu_culling
            .iter()
            .map(|(_, main)| *main)
            .chain(class.entities_gpu_culling.keys().copied())
    })
}

/// Whether the stock shadow queue places a mesh in a shadow view: the mesh casts shadows
/// (`RenderMeshInstanceFlags::SHADOW_CASTER`, cleared by `NotShadowCaster`) and its render layers
/// intersect the shadow view's own (`bevy_pbr` `queue_shadows`). A mesh the queue rejects for either
/// is not part of that view's domain: its missing pipeline is not a missing pass, and the
/// requirement that a member of the domain be queued is not relaxed.
fn shadow_required(
    casts_shadows: bool,
    mesh_layers: Option<&RenderLayers>,
    view_layers: Option<&RenderLayers>,
) -> bool {
    casts_shadows
        && view_layers
            .cloned()
            .unwrap_or_default()
            .intersects(&mesh_layers.cloned().unwrap_or_default())
}

/// The mesh and material resources the proof reads for one entity.
#[derive(SystemParam)]
pub(super) struct DrawResources<'w> {
    mesh_instances: Option<Res<'w, RenderMeshInstances>>,
    material_instances: Option<Res<'w, RenderMaterialInstances>>,
    mesh_allocator: Res<'w, MeshAllocator>,
    render_meshes: Res<'w, RenderAssets<RenderMesh>>,
    materials: Res<'w, ErasedRenderAssets<PreparedMaterial>>,
    bind_groups: Option<Res<'w, MaterialBindGroupAllocators>>,
}

impl DrawResources<'_> {
    /// Match the extracted native instances to a waiting surface's declared replacement payload.
    pub(super) fn payload_matches(
        &self,
        main: MainEntity,
        mesh: bevy::asset::AssetId<Mesh>,
        material: bevy::asset::UntypedAssetId,
    ) -> bool {
        self.mesh_instances
            .as_ref()
            .and_then(|instances| instances.mesh_asset_id(main))
            == Some(mesh)
            && self.material_instances.as_ref().is_some_and(|instances| {
                instances
                    .instances
                    .get(&main)
                    .is_some_and(|instance| instance.asset_id == material)
            })
    }

    /// Whether the stock queue would place `main` in the shadow view whose own layers are
    /// `view_layers` ([`shadow_required`]). A mesh the render world has no queue data for yet is
    /// still required: the queue waits for it rather than rejecting it, and its missing geometry
    /// is counted by [`Self::entity_facts`].
    fn shadow_required(&self, main: MainEntity, view_layers: Option<&RenderLayers>) -> bool {
        let Some(data) = self
            .mesh_instances
            .as_ref()
            .and_then(|instances| instances.render_mesh_queue_data(main))
        else {
            return true;
        };
        shadow_required(
            data.flags()
                .contains(RenderMeshInstanceFlags::SHADOW_CASTER),
            data.render_layers.as_ref(),
            view_layers,
        )
    }

    /// The geometry and material facts of one entity, following what the draw commands themselves
    /// read: the mesh must be in the allocator and prepared; the material must be reachable through
    /// the same chain `SetMaterialBindGroup` walks (instance, allocator for the material type,
    /// prepared material, slab for its bind group, the bind group itself), or the draw is skipped.
    /// Returns the prepared material too, for the pass checks.
    pub(super) fn entity_facts(
        &self,
        main: MainEntity,
    ) -> (EntityFacts, Option<&PreparedMaterial>) {
        let mesh = self
            .mesh_instances
            .as_ref()
            .and_then(|instances| instances.mesh_asset_id(main));
        let instance = self
            .material_instances
            .as_ref()
            .and_then(|instances| instances.instances.get(&main));
        let prepared = instance.and_then(|instance| self.materials.get(instance.asset_id));
        let bind_group_ready = instance.zip(prepared).is_some_and(|(instance, prepared)| {
            self.bind_groups
                .as_ref()
                .and_then(|allocators| allocators.get(&instance.asset_id.type_id()))
                .and_then(|allocator| allocator.get(prepared.binding.group))
                .is_some_and(|slab| slab.bind_group().is_some())
        });
        if !bind_group_ready {
            let step = match (instance, prepared) {
                (None, _) => "no material instance",
                (Some(_), None) => "material not prepared",
                (Some(instance), Some(prepared)) => match self
                    .bind_groups
                    .as_ref()
                    .and_then(|allocators| allocators.get(&instance.asset_id.type_id()))
                {
                    None => "no allocator for the material type",
                    Some(allocator) => match allocator.get(prepared.binding.group) {
                        None => "no slab for the binding group",
                        Some(_) => "slab has no bind group",
                    },
                },
            };
            debug!(entity = ?main, step, "destination proof: material not drawable");
        }
        let facts = EntityFacts {
            mesh_allocated: mesh.is_some_and(|id| self.mesh_allocator.mesh_slabs(&id).is_some()),
            render_mesh: mesh.is_some_and(|id| self.render_meshes.get(id).is_some()),
            material_prepared: bind_group_ready,
            passes: [None; 5],
        };
        (facts, prepared)
    }
}

/// Whether a required shadow family is still missing its domain. `subviews` is how many shadow
/// subviews depend on the view, `lights_prepared` whether the light preparation has run for the view
/// (it inserts `ViewLightEntities` every frame, with an empty list when no light applies), and
/// `cache_present` whether the shadow pipeline cache exists.
///
/// With subviews, the family needs its cache. Without any, the domain is **proven empty** (an
/// unlit interior, a camera whose render layers exclude the light) only once the lights were
/// prepared for the view; before that the lights are simply not extracted yet, which is waiting, not
/// an empty domain. An empty iterator alone never proves it.
fn shadow_domain_missing(subviews: usize, lights_prepared: bool, cache_present: bool) -> bool {
    if subviews > 0 {
        !cache_present
    } else {
        !lights_prepared
    }
}

/// Evaluates every pending request after the queue step: for the requested view, every visible mesh
/// entity must hold its buffers, mesh, a drawable material bind group and, in each required pass,
/// both a specialized pipeline that resolves and a place in the view's queued phase; with shadows
/// required, the same holds for the entities of the view's dependent shadow subviews. A required
/// pass whose cache is absent is missing, not vacuously fine. A view the render world does not
/// extract (inactive, not yet spawned) is not evaluated, and the request stays
/// [`super::destination::Missing::NotEvaluated`].
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn evaluate_destinations(
    extracted: Res<ExtractedRequests>,
    mut evaluations: ResMut<Evaluations>,
    frame: Res<RenderFrame>,
    draw: DrawResources,
    phases: QueuePhases,
    forward: Option<Res<SpecializedMaterialPipelineCache>>,
    prepass: Option<Res<SpecializedPrepassMaterialPipelineCache>>,
    shadow: Option<Res<SpecializedShadowMaterialPipelineCache>>,
    pipelines: Res<PipelineCache>,
    views: Query<(
        &ExtractedView,
        &RenderVisibleEntities,
        Option<&ViewLightEntities>,
    )>,
    lights: Query<&RenderShadowMapVisibleEntities>,
    shadow_views: Query<(&ExtractedView, Option<&RenderLayers>), With<LightEntity>>,
) {
    if extracted.list.is_empty() {
        return;
    }
    if draw.mesh_instances.is_none() || draw.material_instances.is_none() {
        evaluations.map.clear();
        return;
    }

    for pending in extracted.list.iter() {
        let request = &pending.request;
        let wanted = request.passes;
        let Some((view, visible, view_lights)) = views
            .iter()
            .find(|(view, _, _)| view.retained_view_entity == request.key.view.view)
        else {
            evaluations.map.remove(&pending.id);
            continue;
        };
        let mut proof = ProofAccumulator::default();
        let wants_prepass = wanted.contains(PassFamily::DepthPrepass)
            || wanted.contains(PassFamily::NormalPrepass)
            || wanted.contains(PassFamily::MotionPrepass);
        let forward_members = wanted
            .contains(PassFamily::Forward)
            .then(|| phases.forward(&view.retained_view_entity));
        let prepass_members = wants_prepass.then(|| phases.prepass(&view.retained_view_entity));
        for main in visible_meshes(visible) {
            let (mut facts, prepared) = draw.entity_facts(main);
            if let Some(members) = forward_members.as_ref() {
                let pipeline_ok = forward
                    .as_ref()
                    .and_then(|cache| cache.get(&view.retained_view_entity))
                    .and_then(|by_entity| by_entity.get(&main))
                    .is_some_and(|id| pipeline_ok(&pipelines, *id));
                facts.passes[PassFamily::Forward.index()] =
                    Some(pipeline_ok && members.queued(main));
            }
            // The prepass pipelines exist only for materials that have the pass: opaque and
            // alpha-masked with the prepass enabled, never transmissive. A material without it is
            // not missing a pipeline.
            let has_prepass = prepared.is_some_and(|prepared| {
                let properties = &prepared.properties;
                properties.prepass_enabled
                    && !properties.reads_view_transmission_texture
                    && matches!(
                        properties.alpha_mode,
                        AlphaMode::Opaque | AlphaMode::Mask(_) | AlphaMode::AlphaToCoverage
                    )
            });
            if let Some(members) = prepass_members.as_ref()
                && has_prepass
            {
                let queued = prepass
                    .as_ref()
                    .and_then(|cache| cache.get(&view.retained_view_entity))
                    .and_then(|by_entity| by_entity.get(&main))
                    .is_some_and(|(_, id, _)| pipeline_ok(&pipelines, *id))
                    && members.queued(main);
                for family in [
                    PassFamily::DepthPrepass,
                    PassFamily::NormalPrepass,
                    PassFamily::MotionPrepass,
                ] {
                    if wanted.contains(family) {
                        facts.passes[family.index()] = Some(queued);
                    }
                }
            }
            proof.add(&facts);
        }
        if wanted.contains(PassFamily::Shadow) {
            // The light views that depend on this view: a directional light's cascades carry the
            // camera as their auxiliary entity, a point light's cube faces carry a placeholder
            // and belong to every camera.
            let mut subviews = 0;
            for light in lights.iter() {
                for (retained, visible) in light.subviews.iter() {
                    let depends = retained.auxiliary_entity
                        == view.retained_view_entity.main_entity
                        || retained.auxiliary_entity == MainEntity::from(Entity::PLACEHOLDER);
                    if !depends {
                        continue;
                    }
                    subviews += 1;
                    let members = phases.shadow(retained);
                    // The layers of this very shadow view (cascade, cube face or spot), found by
                    // its retained key: a point or spot light's views are shared between cameras,
                    // so the camera's layers say nothing about what they queue.
                    let view_layers = shadow_views
                        .iter()
                        .find(|(shadow_view, _)| shadow_view.retained_view_entity == *retained)
                        .and_then(|(_, layers)| layers);
                    for main in visible_meshes(visible) {
                        let (mut facts, prepared) = draw.entity_facts(main);
                        if !prepared.is_none_or(|prepared| prepared.properties.shadows_enabled)
                            || !draw.shadow_required(main, view_layers)
                        {
                            continue;
                        }
                        let pipeline_ok = shadow
                            .as_ref()
                            .and_then(|cache| cache.get(retained))
                            .and_then(|by_entity| by_entity.get(&main))
                            .is_some_and(|(id, _)| pipeline_ok(&pipelines, *id));
                        facts.passes[PassFamily::Shadow.index()] =
                            Some(pipeline_ok && members.queued(main));
                        proof.add(&facts);
                    }
                }
            }
            if shadow_domain_missing(subviews, view_lights.is_some(), shadow.is_some()) {
                proof.mark_pass_missing(PassFamily::Shadow);
            }
        }
        evaluations.map.insert(
            pending.id,
            EvaluatedProof {
                key: request.key,
                frame: *frame,
                extracted_generation: pending.extracted_generation,
                proof: proof.finish(),
            },
        );
    }
}

/// Combines what the render world proved with residency and assets into one report per
/// pending request. Runs only while a request is pending, so an idle run pays nothing.
#[allow(clippy::too_many_arguments)]
fn update_destination_reports(
    requests: Res<DestinationRequests>,
    generations: Res<Generations>,
    results: Res<RenderResults>,
    streaming: Option<Res<StreamingWorld>>,
    pending_under: PendingUnder,
    assets: LandingAssets,
    mut reports: ResMut<DestinationReports>,
) {
    for (id, request) in requests.iter() {
        let (residency, assets_pending) =
            streaming
                .as_deref()
                .map_or((Residency::Pending, 0), |streaming| {
                    let mut residency = Residency::Resident;
                    let mut assets_pending = 0;
                    for cell in request.coverage.iter() {
                        if streaming.is_failed(*cell) {
                            residency = Residency::Failed(*cell);
                            break;
                        }
                        match streaming.resident_root(*cell) {
                            Some(root) => {
                                let inspected = assets.inspect_under(root);
                                assets_pending += pending_under.count(root) + inspected.waiting;
                            }
                            None => residency = Residency::Pending,
                        }
                    }
                    (residency, assets_pending)
                });
        let evaluated = results
            .proofs
            .get(&id)
            .filter(|proof| proof.key == request.key);
        let render = if results.renderer {
            evaluated.map(|proof| proof.proof)
        } else {
            Some(RenderProof::default())
        };
        let facts = DestinationFacts {
            key: request.key,
            current_generation: generations.current(request.key.world),
            extracted_generation: evaluated
                .map_or(request.key.generation, |proof| proof.extracted_generation),
            evaluated: evaluated.map_or(results.frame, |proof| proof.frame),
            last_invalidation: reports.invalidation(request.key.world),
            residency,
            assets_pending,
            render,
        };
        reports.set(DestinationReport {
            key: request.key,
            evaluated_render_frame: facts.evaluated,
            state: destination_state(&facts),
        });
    }
}

/// Installs the protocol: the resources both worlds hold, the render-world evaluation and the
/// main-world combination.
pub struct DestinationReadinessPlugin;

impl Plugin for DestinationReadinessPlugin {
    fn build(&self, app: &mut App) {
        let renderer = app.get_sub_app(RenderApp).is_some();
        app.init_resource::<Generations>()
            .init_resource::<DestinationRequests>()
            .init_resource::<DestinationReports>()
            .insert_resource(RenderResults {
                renderer,
                ..default()
            })
            .add_systems(
                Update,
                update_destination_reports
                    .in_set(DestinationReportsUpdated)
                    .after(crate::streaming::StreamingUpdate)
                    .run_if(|requests: Res<DestinationRequests>| !requests.is_empty()),
            );
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .init_resource::<RenderFrame>()
                .init_resource::<ExtractedRequests>()
                .init_resource::<Evaluations>()
                .init_resource::<RenderInvalidations>()
                .configure_sets(
                    Render,
                    // A slab that gained a material has no bind group until preparation.
                    DestinationSystems::Evaluate
                        .after(RenderSystems::PrepareBindGroups)
                        .before(RenderSystems::Render),
                )
                .add_systems(ExtractSchedule, extract_destination_protocol)
                .add_systems(
                    Render,
                    evaluate_destinations.in_set(DestinationSystems::Evaluate),
                );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap_bridge::destination::{
        DestinationState, InvalidationCause, Missing, PassFamilies, ViewKey, WorldId,
    };

    fn drawn(passes: [Option<bool>; 5]) -> EntityFacts {
        EntityFacts {
            mesh_allocated: true,
            render_mesh: true,
            material_prepared: true,
            passes,
        }
    }

    #[test]
    fn a_fully_prepared_entity_adds_no_missing_fact() {
        let mut proof = ProofAccumulator::default();
        proof.add(&drawn([Some(true), Some(true), None, None, Some(true)]));
        let proof = proof.finish();
        assert_eq!(proof.checked, 1);
        assert_eq!(
            (
                proof.geometry_missing,
                proof.material_missing,
                proof.pass_missing
            ),
            (0, 0, None)
        );
    }

    #[test]
    fn missing_geometry_material_and_passes_are_counted_and_the_first_pass_is_named() {
        let mut proof = ProofAccumulator::default();
        proof.add(&EntityFacts {
            mesh_allocated: false,
            ..drawn([Some(true), None, None, None, None])
        });
        proof.add(&EntityFacts {
            render_mesh: false,
            ..drawn([Some(true), None, None, None, None])
        });
        proof.add(&EntityFacts {
            material_prepared: false,
            ..drawn([Some(true), None, None, None, None])
        });
        proof.add(&drawn([Some(true), None, None, None, Some(false)]));
        proof.add(&drawn([Some(true), Some(false), None, None, Some(false)]));
        let proof = proof.finish();
        assert_eq!(proof.checked, 5);
        assert_eq!(proof.geometry_missing, 2);
        assert_eq!(proof.material_missing, 1);
        assert_eq!(proof.pass_missing, Some(PassFamily::DepthPrepass));
    }

    /// A required family with nothing to carry its pipelines is missing, with no entity to blame.
    #[test]
    fn a_required_family_marked_missing_is_named_with_no_entities() {
        let mut proof = ProofAccumulator::default();
        proof.mark_pass_missing(PassFamily::Shadow);
        assert_eq!(proof.finish().pass_missing, Some(PassFamily::Shadow));
    }

    fn members(entities: &[u32]) -> PhaseMembers {
        PhaseMembers {
            entities: entities.iter().map(|bits| main_entity(*bits)).collect(),
        }
    }

    fn main_entity(bits: u32) -> MainEntity {
        MainEntity::from(Entity::from_raw_u32(bits).unwrap())
    }

    /// An entity is queued only when a phase holds it: an unrelated queued batch proves nothing.
    #[test]
    fn queue_membership_needs_the_entity_itself() {
        let phase = members(&[1, 2]);
        assert!(phase.queued(main_entity(1)));
        assert!(
            !phase.queued(main_entity(3)),
            "another entity is queued, not this one"
        );
        let mut merged = members(&[1]);
        merged.merge(members(&[5]));
        assert!(merged.queued(main_entity(5)) && !merged.queued(main_entity(6)));
    }

    #[test]
    fn exact_membership_reads_live_views_without_copying_unrelated_phase_entities() {
        use bevy::{
            core_pipeline::core_3d::{Opaque3dBatchSetKey, Opaque3dBinKey},
            render::{
                batching::gpu_preprocessing::GpuPreprocessingMode,
                mesh::allocator::MeshSlabs,
                render_phase::{BinnedRenderPhaseType, DrawFunctionId, InputUniformIndex},
            },
        };
        let view = RetainedViewEntity::new(main_entity(1000), None, 0);
        let other_view = RetainedViewEntity::new(main_entity(1001), None, 0);
        let mut phases = ViewBinnedRenderPhases::<Opaque3d>::default();
        phases.prepare_for_new_frame(view, GpuPreprocessingMode::None);
        let phase = phases.get_mut(&view).unwrap();
        let batch_key = Opaque3dBatchSetKey {
            pipeline: CachedRenderPipelineId::INVALID,
            draw_function: DrawFunctionId(0),
            material_bind_group_index: None,
            slabs: MeshSlabs::default(),
            lightmap_slab: None,
        };
        let bin_key = Opaque3dBinKey {
            asset_id: Handle::<Mesh>::default().id().untyped(),
        };
        for entity in 1..=512 {
            phase.add(
                batch_key.clone(),
                bin_key.clone(),
                (Entity::PLACEHOLDER, main_entity(entity)),
                InputUniformIndex::default(),
                BinnedRenderPhaseType::NonMesh,
            );
        }
        let mut world = World::new();
        world.insert_resource(phases);
        let mut state = bevy::ecs::system::SystemState::<QueuePhases>::new(&mut world);
        FORWARD_PHASE_WORK.with(|work| work.set((0, 0)));
        {
            let queues = state.get(&world).unwrap();
            assert!(queues.contains_forward(&view, main_entity(1)));
            assert!(!queues.contains_forward(&other_view, main_entity(1)));
            assert!(!queues.contains_forward(&view, main_entity(513)));
            assert!(!queues.contains_prepass(&view, main_entity(1)));
            assert!(!queues.contains_shadow(&view, main_entity(1)));
        }
        FORWARD_PHASE_WORK.with(|work| assert_eq!(work.get(), (0, 0)));
        world
            .resource_mut::<ViewBinnedRenderPhases<Opaque3d>>()
            .get_mut(&view)
            .unwrap()
            .remove(main_entity(1));
        {
            let queues = state.get(&world).unwrap();
            assert!(!queues.contains_forward(&view, main_entity(1)));
            assert!(queues.contains_forward(&view, main_entity(2)));
        }
        FORWARD_PHASE_WORK.with(|work| assert_eq!(work.get(), (0, 0)));
    }

    #[test]
    fn real_non_mesh_phase_membership_tracks_the_exact_carrier_and_removal() {
        use bevy::{
            core_pipeline::core_3d::{Opaque3dBatchSetKey, Opaque3dBinKey},
            render::{
                batching::gpu_preprocessing::GpuPreprocessingMode,
                mesh::allocator::MeshSlabs,
                render_phase::{BinnedRenderPhaseType, DrawFunctionId, InputUniformIndex},
            },
        };
        let carrier = main_entity(1);
        let other = main_entity(2);
        let view = RetainedViewEntity::new(main_entity(10), None, 0);
        let mut phases = ViewBinnedRenderPhases::<Opaque3d>::default();
        // BinnedRenderPhase::new is private; this is Bevy's public retained-view constructor.
        phases.prepare_for_new_frame(view, GpuPreprocessingMode::None);
        let phase = phases.get_mut(&view).unwrap();
        let batch_key = Opaque3dBatchSetKey {
            pipeline: CachedRenderPipelineId::INVALID,
            draw_function: DrawFunctionId(0),
            material_bind_group_index: None,
            slabs: MeshSlabs::default(),
            lightmap_slab: None,
        };
        let mut meshes = Assets::<Mesh>::default();
        let mesh = meshes.add(Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        ));
        let bin_key = Opaque3dBinKey {
            asset_id: mesh.id().untyped(),
        };
        phase.add(
            batch_key.clone(),
            bin_key.clone(),
            (Entity::from_raw_u32(101).unwrap(), carrier),
            InputUniformIndex::default(),
            BinnedRenderPhaseType::NonMesh,
        );
        assert_eq!(phase.non_mesh_items.len(), 1);
        assert!(phase.batchable_meshes.is_empty());
        assert!(phase.unbatchable_meshes.is_empty());
        assert!(phase.multidrawable_meshes.is_empty());
        let members = PhaseMembers::of_binned(Some(phase));
        assert!(members.queued(carrier));
        assert!(!members.queued(other));
        // Retained bins survive frame preparation; proof must read real membership.
        phase.prepare_for_new_frame();
        assert!(PhaseMembers::of_binned(Some(phase)).queued(carrier));
        phase.add(
            batch_key,
            bin_key,
            (Entity::from_raw_u32(102).unwrap(), other),
            InputUniformIndex::default(),
            BinnedRenderPhaseType::NonMesh,
        );
        phase.remove(carrier);
        let members = PhaseMembers::of_binned(Some(phase));
        assert!(
            !members.queued(carrier),
            "a remaining non-mesh bin cannot certify another carrier"
        );
        assert!(members.queued(other));
        phase.remove(other);
        assert!(phase.non_mesh_items.is_empty());
        let members = PhaseMembers::of_binned(Some(phase));
        assert!(!members.queued(carrier) && !members.queued(other));
    }

    /// The shadow domain is missing until the lights were prepared for the view; a prepared view
    /// with no dependent subview is a proven empty domain; subviews need their cache.
    #[test]
    fn an_empty_shadow_domain_counts_only_once_the_lights_were_prepared() {
        assert!(
            shadow_domain_missing(0, false, true),
            "lights not extracted yet"
        );
        assert!(
            !shadow_domain_missing(0, true, false),
            "proven empty, nothing to cache"
        );
        assert!(
            shadow_domain_missing(2, true, false),
            "casters but no cache"
        );
        assert!(!shadow_domain_missing(2, true, true));
    }

    /// The stock shadow queue rejects a mesh that does not cast shadows or whose layers do not
    /// intersect the shadow view's own; such a mesh is outside that view's domain even when the
    /// specialization cache holds a pipeline for it, and a member of the domain is still required
    /// to be queued.
    #[test]
    fn a_mesh_the_shadow_queue_rejects_is_outside_the_domain() {
        let layer = |n: usize| RenderLayers::layer(n);
        // Same cache entry, wrong layer: not required.
        assert!(!shadow_required(true, Some(&layer(1)), Some(&layer(0))));
        // Right layer: required.
        assert!(shadow_required(true, Some(&layer(1)), Some(&layer(1))));
        // Default layers on both sides intersect (layer 0).
        assert!(shadow_required(true, None, None));
        // A view with no layer component is layer 0; a mesh on another layer is rejected.
        assert!(!shadow_required(true, Some(&layer(2)), None));
        // NotShadowCaster: rejected whatever the layers.
        assert!(!shadow_required(false, None, None));
        // A shared (point or spot) view carries its own layers, whatever the camera's.
        assert!(shadow_required(
            true,
            Some(&layer(3)),
            Some(&layer(3).with(4))
        ));
    }

    /// A pass the entity does not need (`None`) is never reported missing.
    #[test]
    fn a_pass_an_entity_does_not_need_is_not_missing() {
        let mut proof = ProofAccumulator::default();
        proof.add(&drawn([None; 5]));
        assert_eq!(proof.finish().pass_missing, None);
    }

    #[test]
    fn the_wanted_families_come_out_in_family_order() {
        let wanted = PassFamilies::NONE
            .with(PassFamily::Shadow)
            .with(PassFamily::Forward)
            .with(PassFamily::NormalPrepass);
        assert_eq!(
            wanted.iter().collect::<Vec<_>>(),
            vec![
                PassFamily::Forward,
                PassFamily::NormalPrepass,
                PassFamily::Shadow
            ]
        );
        assert!(
            PassFamily::ALL
                .iter()
                .enumerate()
                .all(|(index, family)| family.index() == index)
        );
    }

    /// Without a renderer (no `RenderApp`) facts 3 and 4 hold vacuously and the report comes from
    /// residency and assets alone; the whole chain runs in a headless app.
    #[test]
    fn a_headless_app_reports_ready_once_residency_and_assets_hold() {
        use crate::{
            streaming::StreamingWorld,
            swap_bridge::destination::{DestinationRequest, InvalidationCause, ViewKey, WorldId},
            world::database::CellKey,
        };
        use bevy::render::view::RetainedViewEntity;
        use std::sync::Arc;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_resource::<StreamingWorld>()
            .add_plugins(DestinationReadinessPlugin);
        let cell = CellKey::Interior(9);
        let world = WorldId(1);
        let generation = app
            .world_mut()
            .resource_mut::<Generations>()
            .bump(world, InvalidationCause::Composition);
        let key = DestinationKey {
            view: ViewKey {
                view: RetainedViewEntity::new(Entity::from_raw_u32(1).unwrap().into(), None, 0),
                incarnation: 0,
            },
            world,
            generation,
        };
        app.world_mut()
            .resource_mut::<DestinationRequests>()
            .request(DestinationRequest {
                key,
                passes: PassFamilies::NONE.with(PassFamily::Forward),
                coverage: Arc::from(vec![cell]),
            });
        app.update();
        let generations = app.world().resource::<Generations>();
        let reports = app.world().resource::<DestinationReports>();
        assert!(
            !reports.is_ready(&key, generations),
            "{}",
            reports.describe(&key)
        );
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(cell, root);
        app.update();
        let generations = app.world().resource::<Generations>();
        let reports = app.world().resource::<DestinationReports>();
        assert!(
            reports.is_ready(&key, generations),
            "{}",
            reports.describe(&key)
        );
        // A bump of the world's generation voids the report.
        app.world_mut()
            .resource_mut::<Generations>()
            .bump(world, InvalidationCause::Candidate);
        let generations = app.world().resource::<Generations>();
        let reports = app.world().resource::<DestinationReports>();
        assert!(!reports.is_ready(&key, generations));
    }

    fn report_app(
        coverage: Vec<crate::world::database::CellKey>,
    ) -> (App, RequestId, DestinationKey) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            DestinationReadinessPlugin,
        ));
        app.init_resource::<StreamingWorld>();
        let world = WorldId::interior(9);
        let generation = app
            .world_mut()
            .resource_mut::<Generations>()
            .bump(world, InvalidationCause::Composition);
        let view = app.world_mut().spawn_empty().id();
        let key = DestinationKey {
            view: ViewKey {
                view: RetainedViewEntity::new(view.into(), None, 0),
                incarnation: 0,
            },
            world,
            generation,
        };
        let id = app
            .world_mut()
            .resource_mut::<DestinationRequests>()
            .request(DestinationRequest {
                key,
                passes: PassFamilies::NONE.with(PassFamily::Forward),
                coverage: coverage.into(),
            });
        (app, id, key)
    }

    #[test]
    fn production_reports_need_the_exact_view_generation_and_render_proof() {
        use crate::world::database::CellKey;
        let cell = CellKey::Interior(9);
        let (mut app, id, key) = report_app(vec![cell]);
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(cell, root);
        app.world_mut().resource_mut::<RenderResults>().renderer = true;
        app.update();
        assert_eq!(
            app.world()
                .resource::<DestinationReports>()
                .get(&key)
                .unwrap()
                .state,
            DestinationState::Waiting(Missing::NotEvaluated)
        );
        let mut proof = EvaluatedProof {
            key,
            frame: RenderFrame(3),
            extracted_generation: key.generation,
            proof: RenderProof {
                material_missing: 1,
                checked: 1,
                ..default()
            },
        };
        app.world_mut()
            .resource_mut::<RenderResults>()
            .proofs
            .insert(id, proof.clone());
        app.update();
        assert_eq!(
            app.world()
                .resource::<DestinationReports>()
                .get(&key)
                .unwrap()
                .state,
            DestinationState::Waiting(Missing::Material)
        );
        proof.proof.material_missing = 0;
        proof.proof.pass_missing = Some(PassFamily::Forward);
        app.world_mut()
            .resource_mut::<RenderResults>()
            .proofs
            .insert(id, proof.clone());
        app.update();
        assert_eq!(
            app.world()
                .resource::<DestinationReports>()
                .get(&key)
                .unwrap()
                .state,
            DestinationState::Waiting(Missing::Pass(PassFamily::Forward))
        );
        proof.proof.pass_missing = None;
        let other_view = app.world_mut().spawn_empty().id();
        proof.key.view.view = RetainedViewEntity::new(other_view.into(), None, 0);
        app.world_mut()
            .resource_mut::<RenderResults>()
            .proofs
            .insert(id, proof.clone());
        app.update();
        assert_eq!(
            app.world()
                .resource::<DestinationReports>()
                .get(&key)
                .unwrap()
                .state,
            DestinationState::Waiting(Missing::NotEvaluated)
        );
        proof.key = key;
        app.world_mut()
            .resource_mut::<RenderResults>()
            .proofs
            .insert(id, proof);
        app.update();
        assert!(
            app.world()
                .resource::<DestinationReports>()
                .is_ready(&key, app.world().resource::<Generations>())
        );
        app.world_mut()
            .resource_mut::<Generations>()
            .bump(key.world, InvalidationCause::Candidate);
        app.update();
        assert_eq!(
            app.world()
                .resource::<DestinationReports>()
                .get(&key)
                .unwrap()
                .state,
            DestinationState::Stale
        );
    }

    #[test]
    fn a_missing_streaming_provider_is_pending_not_resident() {
        let (mut app, _, key) = report_app(vec![crate::world::database::CellKey::Interior(9)]);
        app.world_mut().remove_resource::<StreamingWorld>();
        app.update();
        assert_eq!(
            app.world()
                .resource::<DestinationReports>()
                .get(&key)
                .unwrap()
                .state,
            DestinationState::Waiting(Missing::Residency)
        );
    }

    use bevy::asset::{AssetId, UntypedAssetId};

    fn swap_native_world() -> World {
        use bevy::render::{
            renderer::initialize_renderer_without_callbacks, settings::WgpuSettings,
        };
        let settings = WgpuSettings {
            backends: Some(wgpu::Backends::VULKAN),
            ..default()
        };
        let resources = bevy::tasks::block_on(initialize_renderer_without_callbacks(
            wgpu::Backends::VULKAN,
            None,
            &settings,
        ));
        assert_ne!(
            resources.2.device_type,
            wgpu::DeviceType::Cpu,
            "native swap probes require a hardware GPU"
        );
        assert_eq!(resources.2.backend, wgpu::Backend::Vulkan);
        let device = resources.0;
        let queue = resources.1;
        let adapter = resources.3;
        let pipelines = PipelineCache::new(device.clone(), adapter.clone(), true);
        let mut world = World::new();
        world.insert_resource(device);
        world.insert_resource(queue);
        world.insert_resource(adapter);
        world.insert_resource(pipelines);
        world.init_resource::<bevy::render::mesh::allocator::MeshAllocatorSettings>();
        world.init_resource::<MeshAllocator>();
        world.init_resource::<RenderAssets<RenderMesh>>();
        world.init_resource::<ErasedRenderAssets<PreparedMaterial>>();
        world.init_resource::<RenderMaterialInstances>();
        world.init_resource::<MaterialBindGroupAllocators>();
        world.insert_resource(RenderMeshInstances::CpuBuilding(default()));
        world
    }

    fn swap_native_instance(
        world: &mut World,
        entity: MainEntity,
        mesh: AssetId<Mesh>,
        material: UntypedAssetId,
    ) {
        use bevy::pbr::{
            MeshTransforms, RenderMaterialInstance, RenderMeshInstanceCpu,
            RenderMeshInstanceSharedFlat,
        };
        let shared = RenderMeshInstanceSharedFlat::default();
        let instance = RenderMeshInstanceCpu {
            shared: (&shared).into(),
            transforms: MeshTransforms {
                world_from_local: bevy::math::Affine3::IDENTITY,
                previous_world_from_local: bevy::math::Affine3::IDENTITY,
                flags: 0,
            },
            render_layers: None,
        };
        instance.shared.set_mesh_asset_id(mesh);
        {
            let mut meshes = world.resource_mut::<RenderMeshInstances>();
            let RenderMeshInstances::CpuBuilding(instances) = &mut *meshes else {
                unreachable!()
            };
            instances.insert(entity, instance);
        }
        world
            .resource_mut::<RenderMaterialInstances>()
            .instances
            .insert(
                entity,
                RenderMaterialInstance {
                    asset_id: material,
                    last_change_tick: bevy::ecs::change_detection::Tick::new(1),
                },
            );
    }

    fn swap_native_pending_surface(world: &mut World) -> MainEntity {
        use crate::swap_bridge::{SwapPrepared, SwapSurface, SwapWaiting};
        let mut main = MainWorld::default();
        let entity = main
            .spawn((
                Mesh3d(Handle::default()),
                SwapSurface::new(Handle::<StandardMaterial>::default().id().untyped(), []),
                SwapWaiting,
                SwapPrepared { since: 1 },
            ))
            .id();
        world.insert_resource(main);
        world.init_resource::<crate::swap_bridge::SwapBuffers>();
        world.init_resource::<RenderAssets<bevy::render::texture::GpuImage>>();
        entity.into()
    }

    fn swap_native_material_observation(world: &mut World, entity: MainEntity) -> bool {
        // Exercise the actual Extract -> native evaluator path, including already-Prepared input.
        // This fixture has no allocated geometry and makes no complete SwapDrawn claim.
        world
            .resource_mut::<MainWorld>()
            .entity_mut(entity.id())
            .insert(crate::swap_bridge::SwapPrepared { since: 1 });
        let mut extract = Schedule::new(ExtractSchedule);
        extract.add_systems(crate::swap_bridge::swap_bridge);
        extract.run(world);
        let mut evaluate = Schedule::new(Render);
        evaluate.add_systems(crate::swap_bridge::evaluate_swaps);
        evaluate.run(world);
        world
            .resource::<crate::swap_bridge::SwapBuffers>()
            .completed
            .values()
            .find(|result| MainEntity::from(result.declaration.entity) == entity)
            .unwrap()
            .facts
            .material
    }

    #[test]
    #[ignore = "requires a native GPU; run at the renderer integration gate"]
    fn swap_payload_identity_reads_real_native_instances_and_rejects_replacements() {
        let mut world = swap_native_world();
        let entity = swap_native_pending_surface(&mut world);
        let mesh = Handle::<Mesh>::default().id();
        let material = Handle::<StandardMaterial>::default().id().untyped();
        swap_native_instance(&mut world, entity, mesh, material);
        let mut state = bevy::ecs::system::SystemState::<DrawResources>::new(&mut world);
        assert!(
            state
                .get(&world)
                .unwrap()
                .payload_matches(entity, mesh, material)
        );
        let mut assets = Assets::<Mesh>::default();
        let other_mesh = assets.add(Mesh::from(Cuboid::default()));
        assert!(
            !state
                .get(&world)
                .unwrap()
                .payload_matches(entity, other_mesh.id(), material)
        );
        let mut materials = Assets::<StandardMaterial>::default();
        let other_material = materials.add(StandardMaterial::default());
        assert!(!state.get(&world).unwrap().payload_matches(
            entity,
            mesh,
            other_material.id().untyped()
        ));
        swap_native_instance(
            &mut world,
            entity,
            other_mesh.id(),
            other_material.id().untyped(),
        );
        assert!(
            !state
                .get(&world)
                .unwrap()
                .payload_matches(entity, mesh, material)
        );
        assert!(state.get(&world).unwrap().payload_matches(
            entity,
            other_mesh.id(),
            other_material.id().untyped()
        ));
        world
            .resource_mut::<RenderMaterialInstances>()
            .instances
            .remove(&entity);
        assert!(!state.get(&world).unwrap().payload_matches(
            entity,
            other_mesh.id(),
            other_material.id().untyped()
        ));
        swap_native_instance(&mut world, entity, mesh, material);
        world.insert_resource(RenderMeshInstances::CpuBuilding(default()));
        assert!(
            !state
                .get(&world)
                .unwrap()
                .payload_matches(entity, mesh, material)
        );
    }

    #[test]
    #[ignore = "requires a native GPU; run at the renderer integration gate"]
    fn swap_native_material_chain_requires_instance_allocator_slab_and_actual_bind_group() {
        use bevy::{
            pbr::MaterialBindGroupAllocator,
            render::{
                render_resource::{
                    BindGroupLayoutDescriptor, BindingResources, PreparedBindGroup,
                    UnpreparedBindGroup,
                },
                renderer::RenderDevice,
            },
        };
        let mut world = swap_native_world();
        let entity = swap_native_pending_surface(&mut world);
        let material = Handle::<StandardMaterial>::default().id().untyped();
        swap_native_instance(&mut world, entity, Handle::<Mesh>::default().id(), material);
        let device = world.resource::<RenderDevice>().clone();
        let layout = BindGroupLayoutDescriptor::new("swap native material probe", &[]);
        let gpu_layout = world
            .resource::<PipelineCache>()
            .get_bind_group_layout(&layout);
        let prepared_group = || PreparedBindGroup {
            bindings: BindingResources(vec![]),
            bind_group: device.create_bind_group(
                Some("swap native material probe"),
                &gpu_layout,
                &[],
            ),
        };
        let mut allocator = MaterialBindGroupAllocator::new(
            &device,
            "swap native material probe",
            None,
            layout.clone(),
            None,
        );
        let binding = allocator.allocate_prepared(prepared_group());
        world
            .resource_mut::<MaterialBindGroupAllocators>()
            .insert(material.type_id(), allocator);
        world
            .resource_mut::<ErasedRenderAssets<PreparedMaterial>>()
            .insert(
                material,
                PreparedMaterial {
                    binding,
                    properties: std::sync::Arc::new(default()),
                    shadow_cache_untracked_texture: false,
                },
            );
        assert!(swap_native_material_observation(&mut world, entity));

        let instance = world
            .resource_mut::<RenderMaterialInstances>()
            .instances
            .remove(&entity)
            .unwrap();
        assert!(!swap_native_material_observation(&mut world, entity));
        world
            .resource_mut::<RenderMaterialInstances>()
            .instances
            .insert(entity, instance);
        let prepared = world
            .resource_mut::<ErasedRenderAssets<PreparedMaterial>>()
            .remove(material)
            .unwrap();
        assert!(!swap_native_material_observation(&mut world, entity));
        world
            .resource_mut::<ErasedRenderAssets<PreparedMaterial>>()
            .insert(material, prepared);
        let allocator = world
            .resource_mut::<MaterialBindGroupAllocators>()
            .swap_remove(&material.type_id())
            .unwrap();
        assert!(!swap_native_material_observation(&mut world, entity));
        world
            .resource_mut::<MaterialBindGroupAllocators>()
            .insert(material.type_id(), allocator);
        assert!(swap_native_material_observation(&mut world, entity));

        world
            .resource_mut::<MaterialBindGroupAllocators>()
            .get_mut(&material.type_id())
            .unwrap()
            .free(binding);
        assert!(
            !swap_native_material_observation(&mut world, entity),
            "PreparedMaterial without its slab is insufficient"
        );
        let unprepared = world
            .resource_mut::<MaterialBindGroupAllocators>()
            .get_mut(&material.type_id())
            .unwrap()
            .allocate_unprepared(
                UnpreparedBindGroup {
                    bindings: BindingResources(vec![]),
                },
                &layout,
            );
        world
            .resource_mut::<ErasedRenderAssets<PreparedMaterial>>()
            .get_mut(material)
            .unwrap()
            .binding = unprepared;
        assert!(
            world
                .resource::<MaterialBindGroupAllocators>()
                .get(&material.type_id())
                .unwrap()
                .get(unprepared.group)
                .is_some()
        );
        assert!(
            !swap_native_material_observation(&mut world, entity),
            "a real slab without a bind group is insufficient"
        );

        // Restore an actual GPU bind group through the native allocator, not only metadata.
        let restored = {
            let mut allocators = world.resource_mut::<MaterialBindGroupAllocators>();
            let allocator = allocators.get_mut(&material.type_id()).unwrap();
            allocator.free(unprepared);
            allocator.allocate_prepared(prepared_group())
        };
        world
            .resource_mut::<ErasedRenderAssets<PreparedMaterial>>()
            .get_mut(material)
            .unwrap()
            .binding = restored;
        assert!(swap_native_material_observation(&mut world, entity));
    }

    #[test]
    #[ignore = "requires a native GPU; run at the renderer integration gate"]
    fn swap_settled_empty_render_discards_old_results_without_native_entity_or_phase_work() {
        let mut world = swap_native_world();
        swap_native_pending_surface(&mut world);
        let mut extract = Schedule::new(ExtractSchedule);
        extract.add_systems(crate::swap_bridge::swap_bridge);
        let mut evaluate = Schedule::new(Render);
        evaluate.add_systems(crate::swap_bridge::evaluate_swaps);
        extract.run(&mut world);
        evaluate.run(&mut world);
        assert_eq!(
            world
                .resource::<crate::swap_bridge::SwapBuffers>()
                .completed
                .len(),
            1
        );
        let entity = world
            .resource::<crate::swap_bridge::SwapBuffers>()
            .completed
            .values()
            .next()
            .unwrap()
            .declaration
            .entity;
        world
            .resource_mut::<MainWorld>()
            .entity_mut(entity)
            .remove::<crate::swap_bridge::SwapWaiting>();
        extract.run(&mut world);
        assert!(
            world
                .resource::<crate::swap_bridge::SwapBuffers>()
                .inputs
                .is_empty()
        );
        evaluate.run(&mut world);
        assert!(
            world
                .resource::<crate::swap_bridge::SwapBuffers>()
                .completed
                .is_empty()
        );
        // Zero per-entity/phase calls follows from evaluate_swaps returning before its input loop;
        // this checks buffer lifecycle, not a runtime timing or allocation measurement.
    }

    #[test]
    #[ignore = "requires a native GPU and pipeline; run at the renderer integration gate"]
    fn swap_native_forward_evidence_requires_visibility_queue_and_pipeline_in_the_same_view() {
        use bevy::{
            core_pipeline::core_3d::{Opaque3dBatchSetKey, Opaque3dBinKey},
            render::{
                batching::gpu_preprocessing::GpuPreprocessingMode,
                mesh::allocator::MeshSlabs,
                render_phase::{BinnedRenderPhaseType, DrawFunctionId, InputUniformIndex},
                render_resource::{RenderPipelineDescriptor, TextureFormat, VertexState},
            },
            shader::Shader,
        };
        let mut world = swap_native_world();
        let entity = main_entity(1);
        let a = RetainedViewEntity::new(main_entity(10), None, 0);
        let b = RetainedViewEntity::new(main_entity(11), None, 0);
        let shader = Handle::<Shader>::default();
        let pipeline = {
            let mut cache = world.resource_mut::<PipelineCache>();
            cache.set_shader(shader.id(), Shader::from_wgsl("@vertex fn vertex() -> @builtin(position) vec4<f32> { return vec4<f32>(0., 0., 0., 1.); }", "swap_native_probe.wgsl"));
            let pipeline = cache.queue_render_pipeline(RenderPipelineDescriptor {
                vertex: VertexState {
                    shader,
                    ..default()
                },
                depth_stencil: Some(bevy::render::render_resource::DepthStencilState {
                    format: TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(bevy::render::render_resource::CompareFunction::Always),
                    stencil: default(),
                    bias: default(),
                }),
                ..default()
            });
            cache.process_queue();
            assert!(matches!(
                cache.get_render_pipeline_state(pipeline),
                CachedPipelineState::Ok(_)
            ));
            pipeline
        };
        let mut specialized = SpecializedMaterialPipelineCache::default();
        specialized.entry(a).or_default().insert(entity, pipeline);
        specialized.entry(b).or_default().insert(entity, pipeline);
        world.insert_resource(specialized);
        let visible = || {
            let mut visible = RenderVisibleEntities::default();
            visible.classes.insert(
                std::any::TypeId::of::<Mesh3d>(),
                RenderVisibleEntitiesClass {
                    entities_cpu_culling: vec![(Entity::PLACEHOLDER, entity)],
                    ..default()
                },
            );
            visible
        };
        for (view, visible) in [(a, visible()), (b, RenderVisibleEntities::default())] {
            world.spawn((
                ExtractedView {
                    retained_view_entity: view,
                    clip_from_view: Mat4::IDENTITY,
                    world_from_view: GlobalTransform::IDENTITY,
                    clip_from_world: None,
                    target_format: TextureFormat::Rgba8Unorm,
                    viewport: UVec4::new(0, 0, 1, 1),
                    color_grading: default(),
                    invert_culling: false,
                },
                visible,
            ));
        }
        let mut phases = ViewBinnedRenderPhases::<Opaque3d>::default();
        phases.prepare_for_new_frame(a, GpuPreprocessingMode::None);
        phases.prepare_for_new_frame(b, GpuPreprocessingMode::None);
        let add = |phase: &mut BinnedRenderPhase<Opaque3d>| {
            phase.add(
                Opaque3dBatchSetKey {
                    pipeline,
                    draw_function: DrawFunctionId(0),
                    material_bind_group_index: None,
                    slabs: MeshSlabs::default(),
                    lightmap_slab: None,
                },
                Opaque3dBinKey {
                    asset_id: Handle::<Mesh>::default().id().untyped(),
                },
                (Entity::PLACEHOLDER, entity),
                InputUniformIndex::default(),
                BinnedRenderPhaseType::NonMesh,
            )
        };
        world.insert_resource(phases);
        type Params<'w, 's> = (
            Query<'w, 's, (&'static ExtractedView, &'static RenderVisibleEntities)>,
            QueuePhases<'w>,
            Res<'w, SpecializedMaterialPipelineCache>,
            Res<'w, PipelineCache>,
        );
        let mut state = bevy::ecs::system::SystemState::<Params>::new(&mut world);
        let read = |state: &mut bevy::ecs::system::SystemState<Params>, world: &World| {
            let (views, queues, specialized, pipelines) = state.get(world).unwrap();
            crate::swap_bridge::gather_view_facts(
                entity,
                &views,
                &queues,
                Some(&specialized),
                &pipelines,
            )
        };
        assert!(
            !read(&mut state, &world).pipeline_ok,
            "visible and compiled but absent from the native phase"
        );
        assert!(read(&mut state, &world).in_view);
        assert!(!read(&mut state, &world).forward_queued);
        add(world
            .resource_mut::<ViewBinnedRenderPhases<Opaque3d>>()
            .get_mut(&b)
            .unwrap());
        assert!(
            !read(&mut state, &world).pipeline_ok,
            "queue in invisible view B cannot combine with visible compiled view A"
        );
        add(world
            .resource_mut::<ViewBinnedRenderPhases<Opaque3d>>()
            .get_mut(&a)
            .unwrap());
        assert!(read(&mut state, &world).pipeline_ok);
        assert!(read(&mut state, &world).forward_queued);
        world
            .resource_mut::<ViewBinnedRenderPhases<Opaque3d>>()
            .get_mut(&a)
            .unwrap()
            .remove(entity);
        assert!(!read(&mut state, &world).pipeline_ok);
    }
}
