//! Pure evaluation of the current destination's residency, asset and render facts.
use super::destination::{
    DestinationKey, DestinationState, Failure, Generation, Handoff, HandoffReports, Missing,
    RenderFrame, RenderProof,
};
use crate::world::database::CellKey;

/// Residency of the coverage cells, as the main world reads it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Residency {
    /// Every coverage cell is resident.
    Resident,
    /// At least one is still streaming in.
    Pending,
    /// A cell failed for good.
    Failed(CellKey),
    /// The cell is resident but a model, terrain or water entity under it failed for good: its
    /// pending markers are gone and its scene hidden, so nothing else shows it is missing.
    AssetsFailed(CellKey),
}

/// What fact 5 contributes to the evaluation of `key` in render frame `frame`: the owner's state when
/// it published **for this key and this frame**, else `None` (missing, another key, or another
/// frame, none of which may stand in).
pub fn handoff_for_frame(
    reports: &HandoffReports,
    key: &DestinationKey,
    frame: RenderFrame,
) -> Option<DestinationState> {
    reports
        .get(key)
        .filter(|report| report.key == *key && report.evaluated_render_frame == frame)
        .map(|report| report.state.clone())
}

/// Every input to one request's state, so the decision is a pure function of a snapshot (tested
/// without a GPU, as [`super::swap_readiness`] is).
#[derive(Clone, Debug)]
pub struct DestinationFacts {
    pub key: DestinationKey,
    pub handoff: Handoff,
    /// The world's generation now, in the main world.
    pub current_generation: Generation,
    /// The generation the render world recorded when it extracted the request.
    pub extracted_generation: Generation,
    /// The render frame whose queue decision this snapshot describes.
    pub evaluated: RenderFrame,
    /// The newest render frame in which the world was invalidated, on the render clock.
    pub last_invalidation: Option<RenderFrame>,
    pub residency: Residency,
    pub assets_pending: usize,
    /// `None` until the render world has evaluated the request.
    pub render: Option<RenderProof>,
    /// The owner's state for this key in the evaluated frame ([`handoff_for_frame`]); `None` when it
    /// did not publish one. Ignored for [`Handoff::NotRequired`].
    pub handoff_report: Option<DestinationState>,
}

/// The decision, in one place: stale first, then explicit failures, then the first missing fact in
/// fact order, then ready.
pub fn destination_state(facts: &DestinationFacts) -> DestinationState {
    let key_generation = facts.key.generation;
    let invalidated_after = facts
        .last_invalidation
        .is_some_and(|invalidated| invalidated > facts.evaluated);
    if facts.current_generation != key_generation
        || facts.extracted_generation != key_generation
        || invalidated_after
    {
        return DestinationState::Stale;
    }
    match &facts.residency {
        Residency::Failed(cell) => return DestinationState::Failed(Failure::CellFailed(*cell)),
        Residency::AssetsFailed(cell) if facts.handoff == Handoff::Required => {
            return DestinationState::Failed(Failure::AssetFailed(*cell));
        }
        Residency::Pending => return DestinationState::Waiting(Missing::Residency),
        Residency::Resident | Residency::AssetsFailed(_) => {}
    }
    if facts.assets_pending > 0 {
        return DestinationState::Waiting(Missing::Assets);
    }
    let Some(render) = facts.render else {
        return DestinationState::Waiting(Missing::NotEvaluated);
    };
    if render.geometry_missing > 0 {
        return DestinationState::Waiting(Missing::Geometry);
    }
    if render.material_missing > 0 {
        return DestinationState::Waiting(Missing::Material);
    }
    if let Some(pass) = render.pass_missing {
        return DestinationState::Waiting(Missing::Pass(pass));
    }
    if facts.handoff == Handoff::Required {
        match &facts.handoff_report {
            Some(DestinationState::Ready) => {}
            Some(DestinationState::Failed(failure @ Failure::HandoffFailed(_))) => {
                return DestinationState::Failed(failure.clone());
            }
            Some(DestinationState::Stale) => return DestinationState::Stale,
            // Missing, a report for another key or frame, or a state fact 5 does not use.
            _ => return DestinationState::Waiting(Missing::Handoff),
        }
    }
    DestinationState::Ready
}

#[cfg(test)]
mod tests {
    use super::super::destination::*;
    use super::*;
    use bevy::{prelude::*, render::view::RetainedViewEntity};
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

    fn handoff_report(key: DestinationKey, frame: u64, state: DestinationState) -> HandoffReport {
        HandoffReport {
            key,
            evaluated_render_frame: RenderFrame(frame),
            state,
        }
    }

    fn proven() -> Option<RenderProof> {
        Some(RenderProof {
            checked: 10,
            ..default()
        })
    }

    fn facts(handoff: Handoff) -> DestinationFacts {
        DestinationFacts {
            key: key(3),
            handoff,
            current_generation: Generation(3),
            extracted_generation: Generation(3),
            evaluated: RenderFrame(10),
            last_invalidation: None,
            residency: Residency::Resident,
            assets_pending: 0,
            render: proven(),
            handoff_report: None,
        }
    }

    #[test]
    fn a_stock_only_destination_with_every_fact_is_ready() {
        assert_eq!(
            destination_state(&facts(Handoff::NotRequired)),
            DestinationState::Ready
        );
    }

    #[test]
    fn the_first_missing_fact_in_fact_order_is_named() {
        let mut f = facts(Handoff::NotRequired);
        f.residency = Residency::Pending;
        f.assets_pending = 2;
        f.render = None;
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Residency)
        );
        f.residency = Residency::Resident;
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Assets)
        );
        f.assets_pending = 0;
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::NotEvaluated)
        );
        f.render = Some(RenderProof {
            geometry_missing: 1,
            material_missing: 1,
            pass_missing: Some(PassFamily::Shadow),
            checked: 5,
        });
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Geometry)
        );
        f.render = Some(RenderProof {
            material_missing: 1,
            pass_missing: Some(PassFamily::Shadow),
            checked: 5,
            ..default()
        });
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Material)
        );
        f.render = Some(RenderProof {
            pass_missing: Some(PassFamily::DepthPrepass),
            checked: 5,
            ..default()
        });
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Pass(PassFamily::DepthPrepass))
        );
        f.render = proven();
        assert_eq!(destination_state(&f), DestinationState::Ready);
    }

    #[test]
    fn a_required_handoff_is_checked_after_residency_assets_and_native_passes() {
        let mut f = facts(Handoff::Required);
        f.residency = Residency::Pending;
        f.assets_pending = 2;
        f.render = None;
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Residency)
        );
        f.residency = Residency::Resident;
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Assets)
        );
        f.assets_pending = 0;
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::NotEvaluated)
        );
        f.render = Some(RenderProof {
            geometry_missing: 1,
            material_missing: 1,
            pass_missing: Some(PassFamily::Shadow),
            checked: 5,
        });
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Geometry)
        );
        f.render = Some(RenderProof {
            material_missing: 1,
            pass_missing: Some(PassFamily::Shadow),
            checked: 5,
            ..default()
        });
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Material)
        );
        f.render = Some(RenderProof {
            pass_missing: Some(PassFamily::DepthPrepass),
            checked: 5,
            ..default()
        });
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Pass(PassFamily::DepthPrepass))
        );
        f.render = proven();
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Handoff)
        );
    }

    /// A settled failure is a failure with its reason, not a ready destination.
    #[test]
    fn a_failed_cell_is_a_failure_and_never_ready() {
        let cell = CellKey::Interior(5);
        let mut f = facts(Handoff::NotRequired);
        f.residency = Residency::Failed(cell);
        assert_eq!(
            destination_state(&f),
            DestinationState::Failed(Failure::CellFailed(cell))
        );
    }

    /// A resident cell with a failed asset is a failure even when nothing is pending and nothing
    /// visible is missing: the failed model's scene is hidden, so no visible mesh can show it.
    #[test]
    fn a_resident_cell_with_a_failed_asset_is_a_failure_and_never_ready() {
        let cell = CellKey::Interior(5);
        let mut f = facts(Handoff::Required);
        f.residency = Residency::AssetsFailed(cell);
        f.assets_pending = 0;
        f.render = Some(RenderProof::default());
        assert_eq!(
            destination_state(&f),
            DestinationState::Failed(Failure::AssetFailed(cell))
        );
    }

    /// A required request waits for its owner's handoff for this exact key.
    #[test]
    fn a_required_handoff_is_waited_for_and_a_failed_one_is_a_failure() {
        let mut f = facts(Handoff::Required);
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Handoff)
        );
        f.handoff_report = Some(DestinationState::Ready);
        assert_eq!(destination_state(&f), DestinationState::Ready);
        f.handoff_report = Some(DestinationState::Failed(Failure::HandoffFailed(
            "no pool".into(),
        )));
        assert_eq!(
            destination_state(&f),
            DestinationState::Failed(Failure::HandoffFailed("no pool".into()))
        );
        f.handoff_report = Some(DestinationState::Stale);
        assert_eq!(destination_state(&f), DestinationState::Stale);
        // A state fact 5 does not use (a whole-destination wait, a cell failure) is "not reported".
        f.handoff_report = Some(DestinationState::Waiting(Missing::Residency));
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Handoff)
        );
        f.handoff_report = Some(DestinationState::Failed(Failure::CellFailed(
            CellKey::Interior(1),
        )));
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Handoff)
        );
    }

    /// A stock-only request ignores whatever the owner reported.
    #[test]
    fn a_not_required_handoff_ignores_the_owner() {
        let mut f = facts(Handoff::NotRequired);
        f.handoff_report = Some(DestinationState::Failed(Failure::HandoffFailed("x".into())));
        assert_eq!(destination_state(&f), DestinationState::Ready);
    }

    /// Fact 5 counts only for the key and the render frame being evaluated: nothing from frame N-1
    /// stands in for frame N, and nothing published for another key does either.
    #[test]
    fn a_handoff_counts_only_for_its_key_and_its_frame() {
        let mut reports = HandoffReports::default();
        assert_eq!(handoff_for_frame(&reports, &key(3), RenderFrame(10)), None);
        reports.publish(handoff_report(key(3), 9, DestinationState::Ready));
        assert_eq!(
            handoff_for_frame(&reports, &key(3), RenderFrame(10)),
            None,
            "frame 9 is not frame 10"
        );
        assert_eq!(
            handoff_for_frame(&reports, &key(3), RenderFrame(9)),
            Some(DestinationState::Ready)
        );
        assert_eq!(
            handoff_for_frame(&reports, &key(4), RenderFrame(9)),
            None,
            "another generation is another key"
        );
        reports.clear(&key(3));
        assert_eq!(handoff_for_frame(&reports, &key(3), RenderFrame(9)), None);
    }

    /// The comparison is on the render clock: an invalidation observed after the evaluated frame
    /// voids the state, one observed in or before it does not.
    #[test]
    fn an_invalidation_after_the_evaluated_frame_makes_the_state_stale() {
        let mut f = facts(Handoff::NotRequired);
        f.last_invalidation = Some(RenderFrame(10));
        assert_eq!(destination_state(&f), DestinationState::Ready);
        f.last_invalidation = Some(RenderFrame(11));
        assert_eq!(destination_state(&f), DestinationState::Stale);
    }

    #[test]
    fn a_moved_generation_makes_the_state_stale_in_either_world() {
        let mut f = facts(Handoff::NotRequired);
        f.current_generation = Generation(4);
        assert_eq!(destination_state(&f), DestinationState::Stale);
        f.current_generation = Generation(3);
        f.extracted_generation = Generation(2);
        assert_eq!(destination_state(&f), DestinationState::Stale);
    }

    #[test]
    fn an_ordinary_destination_ignores_a_failed_asset_fact() {
        let mut f = facts(Handoff::NotRequired);
        f.residency = Residency::AssetsFailed(CellKey::Interior(5));
        assert_eq!(destination_state(&f), DestinationState::Ready);
        f.assets_pending = 1;
        assert_eq!(
            destination_state(&f),
            DestinationState::Waiting(Missing::Assets)
        );
    }

    #[test]
    fn a_stale_required_destination_does_not_accept_an_asset_failure() {
        let mut f = facts(Handoff::Required);
        f.residency = Residency::AssetsFailed(CellKey::Interior(5));
        f.current_generation = Generation(4);
        assert_eq!(destination_state(&f), DestinationState::Stale);
    }
}
