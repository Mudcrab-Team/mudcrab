//! The destination readiness protocol: what a crossing (or any consumer) may wait for before it
//! shows a destination, per view, world and generation.
//!
//! This shared protocol names views,
//! worlds, generations, pass families and four facts.
//!
//! The four facts, in the order a report names the first one missing:
//!
//! 1. residency of the coverage cells (a failed cell is a [`Failure`], never "settled so ready");
//! 2. main-world assets loaded under the destination;
//! 3. geometry and material drawable in the render world for every entity the view will draw;
//! 4. a queued pipeline for every **enabled** pass of the view and its dependent shadow views.
//!
//! "Proof" means the renderer holds the entity ready and queued (buffers, bind groups, pipelines,
//! membership), never GPU completion or readback.
//!
//! Timing: the render world evaluates in its queue step of frame N and the next extract copies the
//! result into the main world, so a [`DestinationReport`] describes a **previously evaluated render
//! frame** ([`DestinationReport::evaluated_render_frame`]) and says so. Every comparison between an
//! evaluation and an invalidation uses the render world's own clock ([`RenderFrame`]), never a
//! main-world frame count.
//!
//! Cost: evaluation visits the pending requests and what the requested views will draw; with no
//! request pending nothing is scanned.

use crate::world::database::CellKey;
use bevy::{prelude::*, render::view::RetainedViewEntity};
use std::{collections::HashMap, sync::Arc};

/// The render world's own frame clock, incremented once per render frame, first in the extract.
#[derive(Resource, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct RenderFrame(pub u64);

/// A view's stable identity plus an incarnation its owner bumps when the view is recreated or its
/// pass set changes. A [`RetainedViewEntity`] alone is not enough: a recreated view can reuse it, and
/// a layer set alone does not identify a view.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ViewKey {
    pub view: RetainedViewEntity,
    pub incarnation: u32,
}

/// An opaque world identity (a destination space). The caller builds it; nothing here decodes it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WorldId(pub u64);

impl WorldId {
    /// The exterior of a worldspace, by its form id.
    pub const fn exterior(worldspace: u32) -> Self {
        Self(worldspace as u64)
    }

    /// An interior cell, by its id, with the top bit set so an interior can never meet an exterior.
    pub const fn interior(cell: u32) -> Self {
        Self((1 << 63) | cell as u64)
    }
}

/// A world's invalidation counter: it moves whenever what a destination is made of may have changed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Generation(pub u64);

/// Who changed what a destination is made of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InvalidationCause {
    /// The destination's composition, roles or crossing changed (the crossing owner).
    Composition,
    /// A replacement candidate, its residency, material or eligibility changed (the replacement
    /// owner).
    Candidate,
    /// A render-only late change, observed in the render world.
    Render,
}

/// The shared invalidation counters, one per world, in the main world. Every owner bumps through
/// this one API, so a consumer compares one number.
#[derive(Resource, Default, Debug)]
pub struct Generations {
    worlds: HashMap<WorldId, Generation>,
}

impl Generations {
    /// The world's current generation (zero for a world nothing has bumped).
    pub fn current(&self, world: WorldId) -> Generation {
        self.worlds.get(&world).copied().unwrap_or_default()
    }

    /// Records that `world`'s destination changed, and returns the new generation. The cause is part
    /// of the call so a log line can say who moved it; the counter does not depend on it.
    pub fn bump(&mut self, world: WorldId, cause: InvalidationCause) -> Generation {
        let generation = self.worlds.entry(world).or_default();
        generation.0 += 1;
        debug!(
            world = world.0,
            generation = generation.0,
            ?cause,
            "destination generation bumped"
        );
        *generation
    }
}

/// Render-world late changes: readiness is revoked in the frame the change is observed, on the render
/// clock, and the next extract reports it. The replacement owner calls [`RenderInvalidations::revoke`];
/// nothing in the main world is mutated.
#[derive(Resource, Default, Debug)]
pub struct RenderInvalidations {
    last: HashMap<WorldId, RenderFrame>,
}

impl RenderInvalidations {
    /// Records an invalidation of `world` observed in render frame `at`.
    pub fn revoke(&mut self, world: WorldId, at: RenderFrame, cause: InvalidationCause) {
        let last = self.last.entry(world).or_default();
        *last = (*last).max(at);
        debug!(
            world = world.0,
            render_frame = at.0,
            ?cause,
            "destination readiness revoked in the render world"
        );
    }

    /// A copy of the record, for the extract to mirror into the main world.
    pub fn snapshot(&self) -> HashMap<WorldId, RenderFrame> {
        self.last.clone()
    }

    /// The newest render frame in which `world` was invalidated, if ever.
    pub fn last(&self, world: WorldId) -> Option<RenderFrame> {
        self.last.get(&world).copied()
    }
}

/// A pass family a view can run. A request names the enabled ones; only those are proven.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PassFamily {
    /// The forward colour pass.
    Forward,
    /// The depth prepass.
    DepthPrepass,
    /// The normal prepass.
    NormalPrepass,
    /// The motion-vector prepass.
    MotionPrepass,
    /// The shadow views that depend on the view (its light cascades and cube faces), with the
    /// variants their entities need (alpha mask, terrain discard, snow).
    Shadow,
}

impl PassFamily {
    const fn bit(self) -> u8 {
        match self {
            Self::Forward => 1,
            Self::DepthPrepass => 2,
            Self::NormalPrepass => 4,
            Self::MotionPrepass => 8,
            Self::Shadow => 16,
        }
    }

    /// The family's place in [`PassFamily::ALL`].
    pub const fn index(self) -> usize {
        match self {
            Self::Forward => 0,
            Self::DepthPrepass => 1,
            Self::NormalPrepass => 2,
            Self::MotionPrepass => 3,
            Self::Shadow => 4,
        }
    }

    /// The families in the order a report names the first missing one.
    pub const ALL: [PassFamily; 5] = [
        Self::Forward,
        Self::DepthPrepass,
        Self::NormalPrepass,
        Self::MotionPrepass,
        Self::Shadow,
    ];
}

/// The set of pass families a request needs proven.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PassFamilies(u8);

impl PassFamilies {
    /// No pass.
    pub const NONE: Self = Self(0);

    /// This set plus `family`.
    pub const fn with(self, family: PassFamily) -> Self {
        Self(self.0 | family.bit())
    }

    /// Whether `family` is in the set.
    pub const fn contains(self, family: PassFamily) -> bool {
        self.0 & family.bit() != 0
    }

    /// The families in the set, in report order.
    pub fn iter(self) -> impl Iterator<Item = PassFamily> {
        PassFamily::ALL
            .into_iter()
            .filter(move |family| self.contains(*family))
    }
}

/// What a report is about: one view, one world, one generation of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DestinationKey {
    pub view: ViewKey,
    pub world: WorldId,
    pub generation: Generation,
}

/// What a consumer asks for. The coverage is explicit: the cells the destination is made of.
#[derive(Clone, Debug)]
pub struct DestinationRequest {
    pub key: DestinationKey,
    /// The passes the view runs and the request needs proven.
    pub passes: PassFamilies,
    /// The destination's cells.
    pub coverage: Arc<[CellKey]>,
}

/// Identifies a request so its consumer can withdraw it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RequestId(u64);

/// The requests that are pending, in the main world. Evaluation costs what these ask for and nothing
/// else; a consumer withdraws its request when it has what it waited for.
#[derive(Resource, Default, Debug)]
pub struct DestinationRequests {
    next: u64,
    requests: HashMap<RequestId, DestinationRequest>,
    /// Moves on every change to the pending set or a pending request's key or scope, and on
    /// nothing else, so an observer skips a frame in which no request changed.
    revision: u64,
}

impl DestinationRequests {
    /// Adds a request.
    pub fn request(&mut self, request: DestinationRequest) -> RequestId {
        self.next += 1;
        let id = RequestId(self.next);
        self.requests.insert(id, request);
        self.revision += 1;
        id
    }

    /// Changes a pending request and returns its key. If anything that decides what a report is
    /// about changed (the view, the passes or the coverage) the world's
    /// generation is bumped and the key carries the new one, so a report made for the old request
    /// can never satisfy the new one. An unchanged request keeps its key and bumps nothing.
    pub fn update(
        &mut self,
        id: RequestId,
        generations: &mut Generations,
        view: ViewKey,
        passes: PassFamilies,
        coverage: Arc<[CellKey]>,
    ) -> Option<DestinationKey> {
        let request = self.requests.get_mut(&id)?;
        let changed =
            request.key.view != view || request.passes != passes || request.coverage != coverage;
        if changed {
            request.key.generation =
                generations.bump(request.key.world, InvalidationCause::Composition);
            request.key.view = view;
            request.passes = passes;
            request.coverage = coverage;
            self.revision += 1;
        }
        Some(request.key)
    }

    /// Moves a pending request to the world's current generation and returns its key. A consumer
    /// that is still waiting calls this each frame: when someone bumped the world (a candidate
    /// changed, a role flipped), what it waits for is a new destination, so its request asks again
    /// under the new generation and no report for the old one can satisfy it. Nothing is bumped here.
    pub fn rebase(&mut self, id: RequestId, generations: &Generations) -> Option<DestinationKey> {
        let request = self.requests.get_mut(&id)?;
        let generation = generations.current(request.key.world);
        if request.key.generation != generation {
            request.key.generation = generation;
            self.revision += 1;
        }
        Some(request.key)
    }

    /// Removes a request; true when it was pending.
    pub fn withdraw(&mut self, id: RequestId) -> bool {
        let removed = self.requests.remove(&id).is_some();
        if removed {
            self.revision += 1;
        }
        removed
    }

    /// The change counter: equal values mean no request was added, withdrawn, re-keyed or
    /// re-scoped in between.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The pending requests.
    pub fn iter(&self) -> impl Iterator<Item = (RequestId, &DestinationRequest)> {
        self.requests.iter().map(|(id, request)| (*id, request))
    }

    /// Whether nothing is pending (the evaluation then scans nothing).
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }
}

/// The first fact a destination is still missing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Missing {
    /// Fact 1: a coverage cell is not resident yet.
    Residency,
    /// Fact 2: main-world assets are still loading under the destination.
    Assets,
    /// Fact 3: a visible entity's mesh buffers or `RenderMesh` are not in the render world.
    Geometry,
    /// Fact 3: a visible entity's material bind group or images are not prepared.
    Material,
    /// Fact 4: no queued pipeline for an enabled pass.
    Pass(PassFamily),
    /// The render world has not evaluated this request yet.
    NotEvaluated,
}

/// Why a destination cannot become ready. Explicit: a failure is never counted as settled.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Failure {
    /// A coverage cell failed to load for good and no other representation keeps its coverage.
    CellFailed(CellKey),
}

/// A destination's state, as of the evaluated render frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DestinationState {
    /// All required facts hold.
    Ready,
    /// The first missing fact.
    Waiting(Missing),
    /// Cannot become ready; carries the reason.
    Failed(Failure),
    /// The generation moved, or an invalidation is newer than the evaluation: what was proven no
    /// longer describes the destination.
    Stale,
}

/// What the render world proved for one request: the counts of facts 3 and 4 over what the view will
/// draw, as of the frame it evaluated.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RenderProof {
    /// Visible entities whose mesh buffers or `RenderMesh` are missing.
    pub geometry_missing: usize,
    /// Visible entities whose material bind group or images are missing.
    pub material_missing: usize,
    /// The first enabled pass family with an entity that has no queued pipeline.
    pub pass_missing: Option<PassFamily>,
    /// How many entities the proof visited (for the log of a hold that timed out).
    pub checked: usize,
}

/// A destination's report: the state and the render frame it was evaluated in. It is **not** the
/// frame in which the main world reads it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DestinationReport {
    pub key: DestinationKey,
    pub evaluated_render_frame: RenderFrame,
    pub state: DestinationState,
}

/// The reports, in the main world.
#[derive(Resource, Default, Debug)]
pub struct DestinationReports {
    reports: HashMap<DestinationKey, DestinationReport>,
    /// The render world's newest invalidation per world, mirrored at every extract, so
    /// [`DestinationReports::is_ready`] sees a late revocation even for a report stored as ready
    /// before it.
    invalidations: HashMap<WorldId, RenderFrame>,
}

impl DestinationReports {
    /// Stores the newest report for its key.
    pub fn set(&mut self, report: DestinationReport) {
        self.reports.insert(report.key, report);
    }

    /// The report for `key`.
    pub fn get(&self, key: &DestinationKey) -> Option<&DestinationReport> {
        self.reports.get(key)
    }

    /// Drops the report for `key`.
    pub fn remove(&mut self, key: &DestinationKey) {
        self.reports.remove(key);
    }

    /// The newest render frame in which the render world reported `world` invalidated.
    pub fn invalidation(&self, world: WorldId) -> Option<RenderFrame> {
        self.invalidations.get(&world).copied()
    }

    /// Mirrors the render world's invalidation record (called by the extract; nothing else writes it).
    pub fn mirror_invalidations(&mut self, invalidations: &RenderInvalidations) {
        self.invalidations = invalidations.snapshot();
    }

    /// The only accept rule: the report is ready, the key's generation is still the world's current
    /// one, and no invalidation the render world has since reported is newer than the frame the
    /// report was evaluated in (on the render clock). A ready report stored before a late revocation
    /// stops being ready the moment the revocation is mirrored.
    pub fn is_ready(&self, key: &DestinationKey, generations: &Generations) -> bool {
        generations.current(key.world) == key.generation
            && self.get(key).is_some_and(|report| {
                report.state == DestinationState::Ready
                    && self
                        .invalidations
                        .get(&key.world)
                        .is_none_or(|invalidated| *invalidated <= report.evaluated_render_frame)
            })
    }

    /// What a consumer logs when it gives up waiting: the state, or that nothing was evaluated.
    pub fn describe(&self, key: &DestinationKey) -> String {
        self.get(key).map_or_else(
            || "no report yet".to_owned(),
            |report| {
                format!(
                    "{:?} (evaluated in render frame {})",
                    report.state, report.evaluated_render_frame.0
                )
            },
        )
    }
}

pub use super::destination_evaluation::{DestinationFacts, Residency, destination_state};

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> ViewKey {
        ViewKey {
            view: RetainedViewEntity::new(Entity::from_raw_u32(1).unwrap().into(), None, 0),
            incarnation: 0,
        }
    }

    fn key(generation: u64) -> DestinationKey {
        DestinationKey {
            view: view(),
            world: WorldId(7),
            generation: Generation(generation),
        }
    }

    #[test]
    fn the_accept_rule_needs_a_ready_report_for_the_current_generation() {
        let mut generations = Generations::default();
        let world = WorldId(7);
        generations.bump(world, InvalidationCause::Composition);
        generations.bump(world, InvalidationCause::Composition);
        generations.bump(world, InvalidationCause::Candidate);
        assert_eq!(generations.current(world), Generation(3));
        let mut reports = DestinationReports::default();
        assert!(!reports.is_ready(&key(3), &generations), "no report yet");
        reports.set(DestinationReport {
            key: key(3),
            evaluated_render_frame: RenderFrame(10),
            state: DestinationState::Ready,
        });
        assert!(reports.is_ready(&key(3), &generations));
        generations.bump(world, InvalidationCause::Candidate);
        assert!(
            !reports.is_ready(&key(3), &generations),
            "a ready report for an older generation is not ready"
        );
    }

    /// A late render-world revocation voids a report that was stored as ready before it.
    #[test]
    fn a_mirrored_invalidation_revokes_a_stored_ready_report() {
        let mut generations = Generations::default();
        let world = WorldId(7);
        for _ in 0..3 {
            generations.bump(world, InvalidationCause::Composition);
        }
        let mut reports = DestinationReports::default();
        reports.set(DestinationReport {
            key: key(3),
            evaluated_render_frame: RenderFrame(10),
            state: DestinationState::Ready,
        });
        assert!(reports.is_ready(&key(3), &generations));
        let mut invalidations = RenderInvalidations::default();
        invalidations.revoke(world, RenderFrame(10), InvalidationCause::Render);
        reports.mirror_invalidations(&invalidations);
        assert!(
            reports.is_ready(&key(3), &generations),
            "an invalidation in the evaluated frame is part of that evaluation"
        );
        invalidations.revoke(world, RenderFrame(11), InvalidationCause::Render);
        reports.mirror_invalidations(&invalidations);
        assert!(!reports.is_ready(&key(3), &generations));
    }

    fn view_with(incarnation: u32) -> ViewKey {
        ViewKey {
            incarnation,
            ..view()
        }
    }

    /// Changing what a request asks for bumps the generation, so the old report cannot satisfy the
    /// new request; an unchanged request bumps nothing.
    #[test]
    fn changing_a_request_bumps_the_generation_and_an_unchanged_one_does_not() {
        let mut generations = Generations::default();
        let world = WorldId(7);
        let first = generations.bump(world, InvalidationCause::Composition);
        let coverage: Arc<[CellKey]> = Arc::from(vec![CellKey::Interior(1)]);
        let passes = PassFamilies::NONE.with(PassFamily::Forward);
        let mut requests = DestinationRequests::default();
        let id = requests.request(DestinationRequest {
            key: DestinationKey {
                view: view(),
                world,
                generation: first,
            },
            passes,
            coverage: coverage.clone(),
        });
        let same = requests
            .update(id, &mut generations, view(), passes, coverage.clone())
            .unwrap();
        assert_eq!(same.generation, first);
        assert_eq!(generations.current(world), first, "nothing changed");
        let more_passes = passes.with(PassFamily::Shadow);
        let widened: Arc<[CellKey]> = Arc::from(vec![CellKey::Interior(1), CellKey::Interior(2)]);
        let mut last = first;
        for (view, passes, coverage) in [
            (view_with(1), passes, coverage.clone()),
            (view_with(1), more_passes, coverage.clone()),
            (view_with(1), more_passes, widened.clone()),
        ] {
            let key = requests
                .update(id, &mut generations, view, passes, coverage)
                .unwrap();
            assert!(key.generation > last, "each change moves the generation");
            assert_eq!(generations.current(world), key.generation);
            last = key.generation;
        }
        assert_eq!(
            requests.update(RequestId(999), &mut generations, view(), passes, coverage),
            None
        );
    }

    #[test]
    fn render_invalidations_keep_the_newest_frame() {
        let mut invalidations = RenderInvalidations::default();
        let world = WorldId(1);
        assert_eq!(invalidations.last(world), None);
        invalidations.revoke(world, RenderFrame(5), InvalidationCause::Render);
        invalidations.revoke(world, RenderFrame(3), InvalidationCause::Candidate);
        assert_eq!(invalidations.last(world), Some(RenderFrame(5)));
    }

    #[test]
    fn requests_are_withdrawn_and_an_idle_set_is_empty() {
        let mut requests = DestinationRequests::default();
        assert!(requests.is_empty());
        let id = requests.request(DestinationRequest {
            key: key(1),
            passes: PassFamilies::NONE
                .with(PassFamily::Forward)
                .with(PassFamily::Shadow),
            coverage: Arc::from(vec![CellKey::Interior(1)]),
        });
        assert!(!requests.is_empty());
        let (_, request) = requests.iter().next().unwrap();
        assert_eq!(
            request.passes.iter().collect::<Vec<_>>(),
            vec![PassFamily::Forward, PassFamily::Shadow]
        );
        assert!(requests.withdraw(id));
        assert!(!requests.withdraw(id));
        assert!(requests.is_empty());
    }
}
