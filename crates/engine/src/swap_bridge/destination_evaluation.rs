//! Pure evaluation of the current destination's residency, asset and render facts.
use super::destination::{
    DestinationKey, DestinationState, Failure, Generation, Missing, RenderFrame, RenderProof,
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
}

/// Every input to one request's state, so the decision is a pure function of a snapshot (tested
/// without a GPU, as [`super::swap_readiness`] is).
#[derive(Clone, Debug)]
pub struct DestinationFacts {
    pub key: DestinationKey,
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
        Residency::Pending => return DestinationState::Waiting(Missing::Residency),
        Residency::Resident => {}
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

    fn proven() -> Option<RenderProof> {
        Some(RenderProof {
            checked: 10,
            ..default()
        })
    }

    fn facts() -> DestinationFacts {
        DestinationFacts {
            key: key(3),
            current_generation: Generation(3),
            extracted_generation: Generation(3),
            evaluated: RenderFrame(10),
            last_invalidation: None,
            residency: Residency::Resident,
            assets_pending: 0,
            render: proven(),
        }
    }

    #[test]
    fn a_stock_only_destination_with_every_fact_is_ready() {
        assert_eq!(destination_state(&facts()), DestinationState::Ready);
    }

    #[test]
    fn the_first_missing_fact_in_fact_order_is_named() {
        let mut f = facts();
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

    /// A settled failure is a failure with its reason, not a ready destination.
    #[test]
    fn a_failed_cell_is_a_failure_and_never_ready() {
        let cell = CellKey::Interior(5);
        let mut f = facts();
        f.residency = Residency::Failed(cell);
        assert_eq!(
            destination_state(&f),
            DestinationState::Failed(Failure::CellFailed(cell))
        );
    }

    /// The comparison is on the render clock: an invalidation observed after the evaluated frame
    /// voids the state, one observed in or before it does not.
    #[test]
    fn an_invalidation_after_the_evaluated_frame_makes_the_state_stale() {
        let mut f = facts();
        f.last_invalidation = Some(RenderFrame(10));
        assert_eq!(destination_state(&f), DestinationState::Ready);
        f.last_invalidation = Some(RenderFrame(11));
        assert_eq!(destination_state(&f), DestinationState::Stale);
    }

    #[test]
    fn a_moved_generation_makes_the_state_stale_in_either_world() {
        let mut f = facts();
        f.current_generation = Generation(4);
        assert_eq!(destination_state(&f), DestinationState::Stale);
        f.current_generation = Generation(3);
        f.extracted_generation = Generation(2);
        assert_eq!(destination_state(&f), DestinationState::Stale);
    }
}
