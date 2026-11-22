//! Source bounds and authored room grouping shared by conversion and visibility.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A verified source shape sphere in the glTF mesh's vertex coordinates.
pub const DRAWABLE_SPHERE_EXTRA: &str = "mudcrabDrawableSphere";

/// A NIF bound in Creation units. Placement metadata uses runtime axes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelSphere {
    pub center: [f32; 3],
    pub radius: f32,
}

impl ModelSphere {
    pub fn valid(self) -> bool {
        self.center.iter().all(|v| v.is_finite()) && self.radius.is_finite() && self.radius >= 0.0
    }

    /// NiBound's ordered enclosing-sphere merge. A zero-radius child is skipped by NiNode.
    pub fn merge(self, other: Self) -> Self {
        if self.radius == 0.0 {
            return other;
        }
        if other.radius == 0.0 {
            return self;
        }
        let delta = [0, 1, 2].map(|i| self.center[i] - other.center[i]);
        let distance_squared = delta.iter().map(|v| v * v).sum::<f32>();
        let difference = other.radius - self.radius;
        if distance_squared <= difference * difference {
            return if difference < 0.0 { self } else { other };
        }
        let distance = distance_squared.sqrt();
        // The NiBound merge keeps the original center for nearly coincident spheres.
        // See the 1e-6 center-update threshold in docs/research/skyrim-native-culling.md.
        let center = if distance > 1.0e-6 {
            let fraction = (distance - difference) * (0.5 / distance);
            [0, 1, 2].map(|i| fraction * delta[i] + other.center[i])
        } else {
            self.center
        };
        Self {
            center,
            radius: (other.radius + distance + self.radius) * 0.5,
        }
    }
}

/// One authored marker. `links` retain source order; Skyrim scans its head-inserted list backwards.
#[derive(Debug, Clone)]
pub struct JoinedRoom {
    pub ref_id: u32,
    pub master: bool,
    pub links: Vec<u32>,
}

/// Resolve each marker independently. Direct masters precede recursive links; cycles terminate.
/// A component with several masters stays several rooms, unlike a connected-component union.
pub fn joined_masters(rooms: &[JoinedRoom]) -> Vec<Option<usize>> {
    let indices: HashMap<_, _> = rooms
        .iter()
        .enumerate()
        .map(|(index, room)| (room.ref_id, index))
        .collect();
    let mut links: Vec<_> = rooms.iter().map(|room| room.links.clone()).collect();
    let mut incoming = vec![0usize; rooms.len()];
    // Creating an explicit master installs reciprocal links before resolving its joined bounds.
    for master in rooms.iter().filter(|room| room.master) {
        for target in master.links.iter().rev().filter_map(|id| indices.get(id)) {
            if !links[*target].contains(&master.ref_id) {
                links[*target].push(master.ref_id);
                incoming[*target] += 1;
            }
        }
    }
    let is_master = |index: usize| rooms[index].master || links[index].is_empty();
    (0..rooms.len())
        .map(|start| {
            if is_master(start) {
                return Some(start);
            }
            let mut seen = HashSet::new();
            let mut pending = vec![start];
            while let Some(room) = pending.pop() {
                if !seen.insert(room) {
                    continue;
                }
                if !rooms[room].master && incoming[room] > 1 {
                    // Native construction order remains unknown through recursive links too.
                    return None;
                }
                let room_links = &links[room];
                if let Some(master) = room_links
                    .iter()
                    .rev()
                    .filter_map(|id| indices.get(id).copied())
                    .find(|&index| is_master(index))
                {
                    return Some(master);
                }
                pending.extend(room_links.iter().filter_map(|id| indices.get(id).copied()));
            }
            None
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_master_selection_preserves_order_and_distinct_masters() {
        let room = |ref_id, master, links| JoinedRoom {
            ref_id,
            master,
            links,
        };
        let rooms = [
            room(1, true, vec![2]),
            room(2, false, vec![1, 3]),
            room(3, true, vec![2]),
            room(4, false, vec![5]),
            room(5, false, vec![4, 2]),
            room(6, false, vec![7]),
            room(7, false, vec![6]),
            room(8, false, vec![]),
        ];
        assert_eq!(
            joined_masters(&rooms),
            [
                Some(0),
                Some(2),
                Some(2),
                Some(2),
                Some(2),
                None,
                None,
                Some(7)
            ]
        );
    }

    #[test]
    fn recursive_joined_links_preserve_ambiguous_ownership() {
        let rooms = [
            JoinedRoom {
                ref_id: 1,
                master: true,
                links: vec![2],
            },
            JoinedRoom {
                ref_id: 2,
                master: false,
                links: vec![],
            },
            JoinedRoom {
                ref_id: 3,
                master: true,
                links: vec![2],
            },
            JoinedRoom {
                ref_id: 4,
                master: false,
                links: vec![2],
            },
        ];
        assert_eq!(joined_masters(&rooms), [Some(0), None, Some(2), None]);
    }

    #[test]
    fn sphere_merge_preserves_containment_and_ordered_enclosure() {
        let a = ModelSphere {
            center: [0.0; 3],
            radius: 2.0,
        };
        let b = ModelSphere {
            center: [8.0, 0.0, 0.0],
            radius: 4.0,
        };
        assert_eq!(
            a.merge(b),
            ModelSphere {
                center: [5.0, 0.0, 0.0],
                radius: 7.0
            }
        );
        assert_eq!(a.merge(ModelSphere { radius: 1.0, ..a }), a);
        assert_eq!(a.merge(ModelSphere { radius: 0.0, ..b }), a);
    }

    #[test]
    fn explicit_master_installs_a_missing_reciprocal_link() {
        let rooms = [
            JoinedRoom {
                ref_id: 1,
                master: true,
                links: vec![2],
            },
            JoinedRoom {
                ref_id: 2,
                master: false,
                links: vec![],
            },
        ];
        assert_eq!(joined_masters(&rooms), [Some(0), Some(0)]);
    }
}
