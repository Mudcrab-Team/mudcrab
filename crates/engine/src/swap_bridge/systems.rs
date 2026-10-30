//! Current render phase membership and destination request extraction.
use super::destination::*;
use bevy::{
    core_pipeline::{
        core_3d::{AlphaMask3d, Opaque3d, Transparent3d},
        prepass::{AlphaMask3dPrepass, Opaque3dPrepass},
    },
    ecs::system::SystemParam,
    pbr::{Shadow, Transmissive3d},
    prelude::*,
    render::{
        MainWorld,
        render_phase::{
            BinnedPhaseItem, BinnedRenderPhase, SortedPhaseItem, SortedRenderPhase,
            ViewBinnedRenderPhases, ViewSortedRenderPhases,
        },
        sync_world::MainEntity,
        view::RetainedViewEntity,
    },
};
use std::collections::{HashMap, HashSet};

/// What the render world proved for one request in one render frame.
#[derive(Clone, Debug)]
pub struct EvaluatedProof {
    pub key: DestinationKey,
    /// The render frame whose queue step this describes.
    pub frame: RenderFrame,
    /// The world's generation the render world recorded when it extracted the request.
    pub extracted_generation: Generation,
    pub proof: RenderProof,
    /// The replacement owner's state for this key **in this frame** (fact 5), if it published one.
    pub handoff: Option<DestinationState>,
}

/// The ordering seam of the render-world frame. The replacement owner adds its handoff publication
/// to [`DestinationSystems::PublishHandoff`] (after the queue and planning steps it depends on, which
/// it orders itself, in `RenderSystems::PhaseSort`); the bridge's evaluation is in
/// [`DestinationSystems::Evaluate`], which runs after it and after `RenderSystems::PrepareBindGroups`
/// (before the passes draw), so it reads this frame's final material bind groups. Nothing here names
/// the owner's code.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DestinationSystems {
    /// Handoff publication for the current render frame.
    PublishHandoff,
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

impl ExtractedRequests {
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&DestinationRequest, Generation)> {
        self.list
            .iter()
            .map(|pending| (&pending.request, pending.extracted_generation))
    }
}

/// Render world: the newest evaluation of each pending request.
#[derive(Resource, Default)]
pub(crate) struct Evaluations {
    map: HashMap<RequestId, EvaluatedProof>,
}

/// Increments the render clock, copies the pending requests into the render world and the previous
/// evaluation (with the handoff and invalidation records of the requested keys) back into the main
/// world.
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

/// The entities one render phase of one view holds right now: the batchable and unbatchable bins,
/// the multidrawable batch sets (through the read-only accessor the vendored `bevy_render` adds) and
/// the sorted items. An entity absent from all of them is not queued, whatever else the phase holds.
#[derive(Default)]
pub(crate) struct PhaseMembers {
    entities: HashSet<MainEntity>,
}

impl PhaseMembers {
    fn extend(&mut self, entities: impl IntoIterator<Item = MainEntity>) {
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
