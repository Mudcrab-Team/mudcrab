//! Pure demand ordering. Inputs must contain only currently relevant work.
//!
//! This estimates view-facing footprint from bounds, not frustum visibility or file cost.
//! Callers retain ownership of readiness, cancellation, quotas and scheduling progress.

use bevy::math::DVec3;
use std::cmp::Ordering;

pub(super) const AGED_SERVICE_INTERVAL: u64 = 8;
/// Initial protection distance in runtime units: one exterior cell from the bounds surface.
pub(super) const COLLISION_PROTECTION_DISTANCE: f64 = 4096.0;

#[derive(Debug, Clone, Copy)]
pub(super) struct PriorityView {
    pub position: DVec3,
    pub forward: DVec3,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DemandPriority {
    protected_collision: bool,
    coarse_terrain: bool,
    view_facing: bool,
    projected_radius: f64,
    surface_distance: f64,
    sequence: u64,
    center: [f64; 3],
}

impl DemandPriority {
    pub(super) fn new(
        view: PriorityView,
        center: DVec3,
        bounds_radius: Option<f64>,
        collision_candidate: bool,
        sequence: u64,
    ) -> Self {
        let radius = bounds_radius.filter(|radius| radius.is_finite() && *radius > 0.0);
        let offset = center - view.position;
        let distance = offset.length();
        let forward_length = view.forward.length();
        let valid = view.position.is_finite()
            && view.forward.is_finite()
            && center.is_finite()
            && distance.is_finite()
            && forward_length.is_finite()
            && forward_length > 0.0;
        let radius = radius.unwrap_or(0.0);
        let surface_distance = if valid {
            (distance - radius).max(0.0)
        } else {
            f64::INFINITY
        };
        // A forward hemisphere expanded by the bounding sphere is deliberately conservative.
        // A sphere containing the camera is relevant regardless of its center's direction.
        let depth = if valid {
            offset.dot(view.forward / forward_length)
        } else {
            f64::NEG_INFINITY
        };
        let view_facing = valid && (distance <= radius || depth + radius > 0.0);
        let projected_radius = if view_facing && radius > 0.0 {
            (radius / distance.max(radius)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            coarse_terrain: false,
            protected_collision: valid
                && collision_candidate
                && surface_distance <= COLLISION_PROTECTION_DISTANCE,
            view_facing,
            projected_radius,
            surface_distance,
            sequence,
            // Nonfinite coordinates never enter total ordering as NaNs or signed infinities.
            center: center
                .to_array()
                .map(|value| if value.is_finite() { value } else { 0.0 }),
        }
    }

    /// Coarse terrain has a separate optional lane; broad chunk bounds must not displace
    /// nearby detailed placements. The aged lane still services these relevant chunks.
    pub(super) fn with_coarse_terrain(mut self, coarse: bool) -> Self {
        self.coarse_terrain = coarse;
        self
    }

    /// Less means earlier service. Sequence and coordinates make ordinary ties repeatable.
    pub(super) fn compare(&self, other: &Self) -> Ordering {
        other
            .protected_collision
            .cmp(&self.protected_collision)
            .then_with(|| self.coarse_terrain.cmp(&other.coarse_terrain))
            .then_with(|| other.view_facing.cmp(&self.view_facing))
            .then_with(|| other.projected_radius.total_cmp(&self.projected_radius))
            .then_with(|| self.surface_distance.total_cmp(&other.surface_distance))
            .then_with(|| self.compare_age(other))
    }

    #[cfg(test)]
    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }

    fn compare_age(&self, other: &Self) -> Ordering {
        self.sequence
            .cmp(&other.sequence)
            .then_with(|| self.center[0].total_cmp(&other.center[0]))
            .then_with(|| self.center[1].total_cmp(&other.center[1]))
            .then_with(|| self.center[2].total_cmp(&other.center[2]))
    }
}

/// One aged choice in eight, counted by completed choices rather than frames or failed scans.
/// The aged lane may service a noncollision job; protection is priority, not readiness assurance.
/// Caller-supplied sequences must encode age and candidate order must have deterministic ties.
#[cfg(test)]
pub(super) fn choose_next(candidates: &[DemandPriority], completed_choices: u64) -> Option<usize> {
    let aged_choice = completed_choices % AGED_SERVICE_INTERVAL == AGED_SERVICE_INTERVAL - 1;
    candidates
        .iter()
        .enumerate()
        .min_by(|(left_index, left), (right_index, right)| {
            let order = if aged_choice {
                left.compare_age(right)
            } else {
                left.compare(right)
            };
            order.then_with(|| left_index.cmp(right_index))
        })
        .map(|(index, _)| index)
}

/// Batch repeated selection/removal, returning original candidate indices.
/// Two sorted orders and advancing cursors avoid rescanning the queue for every choice.
pub(super) fn ordered_choices(
    candidates: &[DemandPriority],
    completed_choices: u64,
    count: usize,
) -> Vec<usize> {
    let count = count.min(candidates.len());
    if count == 0 {
        return Vec::new();
    }
    let mut normal: Vec<_> = (0..candidates.len()).collect();
    let mut aged = normal.clone();
    normal.sort_unstable_by(|&left, &right| {
        candidates[left]
            .compare(&candidates[right])
            .then_with(|| left.cmp(&right))
    });
    aged.sort_unstable_by(|&left, &right| {
        candidates[left]
            .compare_age(&candidates[right])
            .then_with(|| left.cmp(&right))
    });
    let mut used = vec![false; candidates.len()];
    let mut normal_cursor = 0;
    let mut aged_cursor = 0;
    let mut choices = completed_choices;
    let mut selected = Vec::with_capacity(count);
    for _ in 0..count {
        let aged_choice = choices % AGED_SERVICE_INTERVAL == AGED_SERVICE_INTERVAL - 1;
        let (order, cursor) = if aged_choice {
            (&aged, &mut aged_cursor)
        } else {
            (&normal, &mut normal_cursor)
        };
        while used[order[*cursor]] {
            *cursor += 1;
        }
        let index = order[*cursor];
        *cursor += 1;
        used[index] = true;
        selected.push(index);
        choices = choices.saturating_add(1);
    }
    selected
}

/// Rank a shared scene by its best relevant placement, retaining its oldest subscriber's age.
/// Recompute after demand changes; callers still deduplicate actual asset requests.
pub(super) fn shared_scene_priority(
    subscribers: impl IntoIterator<Item = DemandPriority>,
) -> Option<DemandPriority> {
    let mut best: Option<DemandPriority> = None;
    let mut oldest = u64::MAX;
    for priority in subscribers {
        oldest = oldest.min(priority.sequence);
        if best.is_none_or(|current| priority.compare(&current).is_lt()) {
            best = Some(priority);
        }
    }
    best.map(|mut priority| {
        priority.sequence = oldest;
        priority
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> PriorityView {
        PriorityView {
            position: DVec3::ZERO,
            forward: DVec3::NEG_Z,
        }
    }

    fn demand(
        center: DVec3,
        radius: Option<f64>,
        collision: bool,
        sequence: u64,
    ) -> DemandPriority {
        DemandPriority::new(view(), center, radius, collision, sequence)
    }

    #[test]
    fn nearby_collision_is_protected_behind_the_camera() {
        let collision = demand(DVec3::new(0.0, 0.0, 100.0), Some(10.0), true, 2);
        let building = demand(DVec3::new(0.0, 0.0, -100.0), Some(80.0), false, 1);
        assert!(collision.compare(&building).is_lt());
        let far_collision = demand(DVec3::new(0.0, 0.0, 9000.0), Some(10.0), true, 0);
        assert!(building.compare(&far_collision).is_lt());
    }

    #[test]
    fn building_footprint_outweighs_clutter_at_similar_distance() {
        let clutter = demand(DVec3::new(0.0, 0.0, -1000.0), Some(2.0), false, 0);
        let building = demand(DVec3::new(0.0, 0.0, -1100.0), Some(100.0), false, 1);
        assert_eq!(choose_next(&[clutter, building], 0), Some(1));
    }

    #[test]
    fn turning_reverses_view_facing_priority() {
        let centers = [DVec3::new(0.0, 0.0, -1000.0), DVec3::new(0.0, 0.0, 1000.0)];
        let priorities = centers.map(|center| demand(center, Some(20.0), false, 0));
        assert_eq!(choose_next(&priorities, 0), Some(0));
        let reversed = PriorityView {
            forward: DVec3::Z,
            ..view()
        };
        let priorities =
            centers.map(|center| DemandPriority::new(reversed, center, Some(20.0), false, 0));
        assert_eq!(choose_next(&priorities, 0), Some(1));
    }

    #[test]
    fn every_eighth_completed_choice_services_old_unknown_bounds() {
        let unknown = demand(DVec3::new(0.0, 0.0, 1000.0), None, false, 1);
        let building = demand(DVec3::new(0.0, 0.0, -1000.0), Some(100.0), false, 2);
        for completed in 0..7 {
            assert_eq!(choose_next(&[building, unknown], completed), Some(0));
        }
        assert_eq!(choose_next(&[building, unknown], 7), Some(1));
        assert_eq!(choose_next(&[building, unknown], 15), Some(1));
        assert_eq!(choose_next(&[], 7), None);
    }

    #[test]
    fn invalid_inputs_fall_back_to_age_and_inside_bounds_stay_finite() {
        let invalid_view = PriorityView {
            forward: DVec3::NAN,
            ..view()
        };
        let first = DemandPriority::new(invalid_view, DVec3::ZERO, Some(10.0), true, 1);
        let second = DemandPriority::new(invalid_view, DVec3::Z, Some(20.0), false, 2);
        assert!(first.compare(&second).is_lt());
        let inside = demand(DVec3::ZERO, Some(100.0), false, 3);
        assert_eq!(inside.projected_radius, 1.0);
        assert_eq!(inside.surface_distance, 0.0);
        assert!(inside.projected_radius.is_finite());
        let invalid_bounds = demand(DVec3::NEG_Z, Some(f64::NAN), false, 4);
        assert_eq!(invalid_bounds.projected_radius, 0.0);
    }

    #[test]
    fn ties_and_rebases_use_stable_coordinates() {
        let center = DVec3::new(-16384.0, 40.0, 8192.0);
        let camera = DVec3::new(-16000.0, 50.0, 8000.0);
        let first = DemandPriority::new(
            PriorityView {
                position: camera,
                ..view()
            },
            center,
            Some(20.0),
            false,
            3,
        );
        let offset = DVec3::new(-12288.0, 0.0, 8192.0);
        let rebased = DemandPriority::new(
            PriorityView {
                position: (camera - offset) + offset,
                ..view()
            },
            (center - offset) + offset,
            Some(20.0),
            false,
            3,
        );
        assert_eq!(first.compare(&rebased), Ordering::Equal);
        let older = DemandPriority::new(
            PriorityView {
                position: camera,
                ..view()
            },
            center,
            Some(20.0),
            false,
            2,
        );
        assert!(older.compare(&first).is_lt());
        let left = demand(DVec3::new(-10.0, 0.0, -100.0), Some(2.0), false, 1);
        let right = demand(DVec3::new(10.0, 0.0, -100.0), Some(2.0), false, 1);
        assert!(left.compare(&right).is_lt());
    }

    #[test]
    fn shared_scene_uses_best_coverage_and_oldest_relevant_age() {
        let old_offscreen = demand(DVec3::new(0.0, 0.0, 1000.0), Some(2.0), false, 1);
        let visible = demand(DVec3::new(0.0, 0.0, -1000.0), Some(100.0), false, 8);
        let shared = shared_scene_priority([old_offscreen, visible]).unwrap();
        assert!(shared.view_facing);
        assert_eq!(shared.projected_radius, visible.projected_radius);
        assert_eq!(shared.sequence(), 1);
        assert!(shared_scene_priority([]).is_none());
    }

    #[test]
    fn nearby_detail_precedes_giant_coarse_bounds_but_aged_service_keeps_coarse_moving() {
        let coarse = demand(DVec3::new(0.0, 0.0, -32768.0), Some(46340.0), false, 1)
            .with_coarse_terrain(true);
        let building = demand(DVec3::new(0.0, 0.0, -1000.0), Some(100.0), false, 2);
        assert_eq!(coarse.projected_radius, 1.0);
        assert!(building.compare(&coarse).is_lt());
        for completed in 0..7 {
            assert_eq!(choose_next(&[coarse, building], completed), Some(1));
        }
        assert_eq!(choose_next(&[coarse, building], 7), Some(0));
        assert_eq!(choose_next(&[coarse, building], 8), Some(1));
        assert_eq!(choose_next(&[coarse], 8), Some(0));
    }

    #[test]
    fn batch_choices_match_repeated_selection_across_lanes_and_budget_boundaries() {
        let candidates = [
            demand(DVec3::new(0.0, 0.0, 100.0), Some(5.0), true, 6),
            demand(DVec3::new(0.0, 0.0, -1000.0), Some(100.0), false, 4),
            demand(DVec3::new(0.0, 0.0, -32768.0), Some(46340.0), false, 2)
                .with_coarse_terrain(true),
            demand(DVec3::new(0.0, 0.0, 1000.0), None, false, 1),
            demand(DVec3::new(0.0, 0.0, -1000.0), Some(2.0), false, 3),
            demand(DVec3::new(0.0, 0.0, -500.0), Some(40.0), true, 5),
            demand(DVec3::new(-100.0, 0.0, -1000.0), Some(20.0), false, 8),
            demand(DVec3::new(100.0, 0.0, -1000.0), Some(20.0), false, 8),
            demand(DVec3::new(100.0, 0.0, -1000.0), Some(20.0), false, 8),
        ];
        for start in (0..=16).chain([u64::MAX - 1, u64::MAX]) {
            for limit in [0, 1, 8, candidates.len(), candidates.len() + 3] {
                let mut remaining: Vec<_> = candidates.iter().copied().enumerate().collect();
                let mut expected = Vec::new();
                let mut completed = start;
                for _ in 0..limit.min(candidates.len()) {
                    let priorities: Vec<_> = remaining.iter().map(|(_, rank)| *rank).collect();
                    let next = choose_next(&priorities, completed).unwrap();
                    expected.push(remaining.remove(next).0);
                    completed = completed.saturating_add(1);
                }
                assert_eq!(
                    ordered_choices(&candidates, start, limit),
                    expected,
                    "start={start}, limit={limit}"
                );
            }
        }
        assert!(ordered_choices(&[], 7, 8).is_empty());
    }
}
