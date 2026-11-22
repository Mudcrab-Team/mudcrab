//! Authored room unions and reference ownership. Visibility volumes remain per view.

use super::{CellVisibility, MeshBounds, Portal, PortalSide, Room};
use bevy::{math::Affine3A, prelude::*};
use rusqlite::Connection;
use shared::room_assignment::{JoinedRoom, ModelSphere, joined_masters};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
enum Rule {
    Automatic,
    None,
    Target(PortalSide),
    Unknown,
}

#[derive(Debug, Clone)]
struct ReferenceRule {
    rule: Rule,
    sphere: Option<ModelSphere>,
    placed: Affine3A,
}

#[derive(Debug, Clone)]
pub(crate) struct Assignments {
    pub complete: bool,
    pub masters: Vec<Option<usize>>,
    joined: Vec<Vec<usize>>,
    rules: HashMap<u32, ReferenceRule>,
}

/// Several possible owners retain the union when native hash iteration order is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Owner {
    Room(usize),
    Portal(u32),
    Unbounded,
    Always,
}

pub(super) struct CachedAssignment {
    pub root: Entity,
    pub transform: Affine3A,
    pub owners: Vec<Owner>,
}

impl Assignments {
    pub(super) fn has_rule(&self, id: u32) -> bool {
        self.rules.contains_key(&id)
    }

    pub(super) fn initial_owners(
        &self,
        id: u32,
        transform: Affine3A,
        rooms: &[Room],
        portals: &[Portal],
    ) -> Vec<Owner> {
        if let Some(ReferenceRule {
            rule: Rule::Target(PortalSide::Room(room)),
            placed,
            ..
        }) = self.rules.get(&id)
            && transform == *placed
        {
            return vec![Owner::Room(*room)];
        }
        self.owners(id, Some(transform), rooms, portals)
    }
    pub(super) fn contains(&self, rooms: &[Room], room: usize, point: Vec3) -> bool {
        self.joined[room]
            .iter()
            .any(|&index| rooms[index].contains(point))
    }

    /// An unbounded skin can deform away from its named room even while its reference is stationary.
    pub(super) fn unbounded_named_skin(&self, id: u32) -> bool {
        matches!(
            self.rules.get(&id),
            Some(ReferenceRule {
                rule: Rule::Target(PortalSide::Room(_)),
                sphere: None,
                ..
            })
        )
    }

    pub(super) fn owners(
        &self,
        id: u32,
        transform: Option<Affine3A>,
        rooms: &[Room],
        portals: &[Portal],
    ) -> Vec<Owner> {
        let Some(rule) = self.rules.get(&id) else {
            return vec![Owner::Always];
        };
        match rule.rule {
            Rule::None | Rule::Unknown => return vec![Owner::Always],
            Rule::Target(PortalSide::Unknown | PortalSide::Unbounded) => {
                return vec![Owner::Always];
            }
            _ => {}
        }
        let Some(sphere) = rule
            .sphere
            .zip(transform)
            .and_then(|(sphere, matrix)| transformed(sphere, matrix))
        else {
            return vec![Owner::Always];
        };
        let point = Vec3::from_array(sphere.center);
        let containing: Vec<_> = (0..rooms.len())
            .filter(|&room| self.masters[room] == Some(room) && self.contains(rooms, room, point))
            .collect();
        if let Rule::Target(PortalSide::Room(room)) = rule.rule
            && containing.contains(&room)
        {
            return vec![owner_for_space(PortalSide::Room(room), sphere, portals)];
        }
        if containing.is_empty() {
            return vec![owner_for_space(PortalSide::Unbounded, sphere, portals)];
        }
        containing
            .into_iter()
            .map(|room| owner_for_space(PortalSide::Room(room), sphere, portals))
            .collect()
    }
}

/// Native NiTransform bounds have one uniform scale. Other affine transforms remain unassigned.
fn transformed(sphere: ModelSphere, matrix: Affine3A) -> Option<ModelSphere> {
    let axes = [
        matrix.matrix3.x_axis,
        matrix.matrix3.y_axis,
        matrix.matrix3.z_axis,
    ];
    // Allow f32 roundoff in a uniform transform. Axis lengths scale linearly, while their
    // dot products scale quadratically, so both tests use the same relative tolerance.
    let scale = axes[0].length();
    if !scale.is_finite()
        || scale <= 0.0
        || axes
            .iter()
            .any(|axis| (axis.length() - scale).abs() > scale * 1.0e-5)
        || [(0, 1), (0, 2), (1, 2)]
            .into_iter()
            .any(|(a, b)| axes[a].dot(axes[b]).abs() > scale * scale * 1.0e-5)
    {
        return None;
    }
    // The largest row sum of |AᵀA| bounds its largest eigenvalue, including
    // the small scale differences and shear admitted by the checks above.
    let axes = axes.map(|axis| axis.as_dvec3());
    let stretch_squared = axes
        .iter()
        .map(|a| axes.iter().map(|b| a.dot(*b).abs()).sum::<f64>())
        .fold(0.0, f64::max);
    let radius = f64::from(sphere.radius) * stretch_squared.sqrt();
    let rounded = radius as f32;
    let sphere = ModelSphere {
        center: matrix
            .transform_point3(Vec3::from_array(sphere.center))
            .to_array(),
        radius: if f64::from(rounded) < radius {
            rounded.next_up()
        } else {
            rounded
        },
    };
    (sphere.valid() && sphere.radius > 0.0).then_some(sphere)
}

fn owner_for_space(space: PortalSide, sphere: ModelSphere, portals: &[Portal]) -> Owner {
    let mut hits = HashSet::new();
    for portal in portals {
        if (space == PortalSide::Unbounded || portal.rooms.contains(&space))
            && sphere_intersects_portal(sphere, portal)
        {
            hits.insert(portal.ref_id);
            if hits.len() > 1 {
                return Owner::Always;
            }
        }
    }
    if let Some(&portal) = hits.iter().next() {
        return Owner::Portal(portal);
    }
    match space {
        PortalSide::Room(room) => Owner::Room(room),
        PortalSide::Unbounded => Owner::Unbounded,
        PortalSide::Unknown => Owner::Always,
    }
}

/// Exact-build RVA fe9860, including its cosf refinement and final normal-axis comparison.
/// This deliberately preserves the native test rather than substituting sphere/rectangle distance.
fn sphere_intersects_portal(sphere: ModelSphere, portal: &Portal) -> bool {
    let [a, b, c, d] = portal.corners.map(|corner| corner.as_dvec3());
    let u = (b - a) * 0.5;
    let v = (d - a) * 0.5;
    let center = ((a + b + c + d) * 0.25).as_vec3();
    let delta = Vec3::from_array(sphere.center) - center;
    let x = delta.dot(u.normalize_or_zero().as_vec3()).abs();
    let y = delta.dot(u.cross(v).normalize_or_zero().as_vec3()).abs();
    let z = delta.dot(v.normalize_or_zero().as_vec3()).abs();
    let half_width = u.length() as f32;
    let half_height = v.length() as f32;
    let radius = sphere.radius;
    if x - radius > half_width || z - radius > half_height || y - radius > 0.0 {
        return false;
    }
    let inset = ((y - radius).abs() / radius).cos() * radius;
    x - inset <= half_width && y - inset <= half_height
}

pub(super) fn draws(
    owners: &[Owner],
    portals: &[Portal],
    visible: &CellVisibility,
    bounds: Option<&MeshBounds>,
) -> bool {
    owners.iter().any(|owner| match *owner {
        Owner::Always => true,
        Owner::Room(room) => visible
            .rooms
            .get(room)
            .is_none_or(|room| room.intersects(bounds)),
        Owner::Unbounded => visible.unbounded.intersects(bounds),
        Owner::Portal(id) => portals
            .iter()
            .filter(|portal| portal.ref_id == id)
            .any(|portal| {
                portal.rooms.iter().any(|&side| {
                    visible
                        .get(side)
                        .is_none_or(|volume| volume.intersects(bounds))
                })
            }),
    })
}

pub(super) fn master_side(
    id: u32,
    indices: &HashMap<u32, usize>,
    masters: &[Option<usize>],
) -> PortalSide {
    indices
        .get(&id)
        .and_then(|&i| masters[i])
        .map(PortalSide::Room)
        .unwrap_or(PortalSide::Unknown)
}

pub(super) fn load(
    conn: &Connection,
    cell: u32,
    rooms: &[Room],
    indices: &HashMap<u32, usize>,
    portals: &mut [Portal],
) -> rusqlite::Result<Option<Assignments>> {
    let supported: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='room_reference_rules') AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='room_model_bounds') AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='room_graph_cells') AND EXISTS(SELECT 1 FROM pragma_table_info('room_links') WHERE name='link_index')",
        [], |row| row.get(0),
    )?;
    if !supported {
        return Ok(None);
    }
    let complete: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_graph_cells WHERE cell_id=?1 AND complete=1)",
        [cell],
        |row| row.get(0),
    )?;
    let mut ordered = conn.prepare_cached(
        "SELECT linked_ref FROM room_links WHERE room_ref=?1 ORDER BY link_index,linked_ref",
    )?;
    let mut flags = conn.prepare_cached("SELECT flags FROM room_bounds WHERE ref_id=?1")?;
    let authored = rooms
        .iter()
        .map(|room| {
            let flags: u32 = flags.query_row([room.ref_id], |row| row.get(0))?;
            Ok(JoinedRoom {
                ref_id: room.ref_id,
                master: ((flags >> 8) & 255) != 0,
                links: ordered
                    .query_map([room.ref_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?,
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let masters = joined_masters(&authored);
    let mut joined = vec![Vec::new(); rooms.len()];
    for (index, master) in masters.iter().enumerate() {
        if let Some(master) = master {
            joined[*master].push(index);
        }
    }
    for portal in portals.iter_mut() {
        for side in &mut portal.rooms {
            if let PortalSide::Room(index) = *side {
                *side = masters[index]
                    .map(PortalSide::Room)
                    .unwrap_or(PortalSide::Unknown);
            }
        }
    }
    let mut statement = conn.prepare_cached(
        "SELECT p.reference_id,p.assignment,p.target_ref,r.pos_x,r.pos_y,r.pos_z,r.rot_x,r.rot_y,r.rot_z,r.scale,b.center_x,b.center_y,b.center_z,b.radius
         FROM room_reference_rules p JOIN \"references\" r ON r.id=p.reference_id
         LEFT JOIN room_model_bounds b ON b.base_form_id=r.base_form_id WHERE p.cell_id=?1",
    )?;
    let rules = statement
        .query_map([cell], |row| {
            let kind: String = row.get(1)?;
            let rule = match kind.as_str() {
                "auto" => Rule::Automatic,
                "none" => Rule::None,
                "target" => Rule::Target(
                    row.get::<_, Option<u32>>(2)?
                        .map(|id| master_side(id, indices, &masters))
                        .unwrap_or(PortalSide::Unknown),
                ),
                _ => Rule::Unknown,
            };
            let position = [row.get(3)?, row.get(4)?, row.get(5)?];
            let rotation = [row.get(6)?, row.get(7)?, row.get(8)?];
            let placed = Affine3A::from_scale_rotation_translation(
                Vec3::splat(row.get(9)?),
                Quat::from_array(shared::coordinates::creation_euler_to_runtime_quaternion(
                    rotation,
                )),
                Vec3::from_array(shared::coordinates::creation_to_runtime_vector(position)),
            );
            let sphere = match row.get::<_, Option<f32>>(13)? {
                Some(radius) => Some(ModelSphere {
                    center: [row.get(10)?, row.get(11)?, row.get(12)?],
                    radius,
                }),
                None => None,
            }
            .filter(|sphere| sphere.valid() && sphere.radius > 0.0);
            Ok((
                row.get(0)?,
                ReferenceRule {
                    rule,
                    sphere,
                    placed,
                },
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(Some(Assignments {
        complete,
        masters,
        joined,
        rules,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a reconverted Gallows Rock database in MUDCRAB_SEAM_DATABASE"]
    fn gallows_rock_seam_model_owners() {
        let database = std::env::var_os("MUDCRAB_SEAM_DATABASE").unwrap();
        let db = Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let graph = super::super::load_room_graph(&db, 86643).unwrap();
        let authored = graph.authored.as_ref().unwrap();
        let owner = graph
            .rooms
            .iter()
            .position(|room| room.ref_id == 0x000A_A079)
            .unwrap();
        for id in [0x0004_2177, 0x0004_2178, 0x0005_A22A] {
            assert!(
                authored.rules[&id].sphere.is_some(),
                "missing model sphere for {id:08X}"
            );
            assert_eq!(
                authored.initial_owners(
                    id,
                    authored.rules[&id].placed,
                    &graph.rooms,
                    &graph.portals
                ),
                [Owner::Room(owner)],
                "retail room ancestor of {id:08X}"
            );
        }
    }

    fn room(id: u32, x: f32) -> Room {
        Room {
            ref_id: id,
            center: Vec3::new(x, 0.0, 0.0),
            rotation: Quat::IDENTITY,
            half: Vec3::splat(4.0),
        }
    }

    fn automatic(sphere: Option<ModelSphere>) -> Assignments {
        Assignments {
            complete: true,
            masters: vec![Some(0), Some(1)],
            joined: vec![vec![0], vec![1]],
            rules: HashMap::from([(
                100,
                ReferenceRule {
                    rule: Rule::Automatic,
                    sphere,
                    placed: Affine3A::IDENTITY,
                },
            )]),
        }
    }

    #[test]
    fn ownership_follows_bound_center_and_current_transform() {
        let rooms = [room(1, 0.0), room(2, 25.0)];
        let assignment = automatic(Some(ModelSphere {
            center: [25.0, 0.0, 0.0],
            radius: 1.0,
        }));
        assert_eq!(
            assignment.initial_owners(100, Affine3A::IDENTITY, &rooms, &[]),
            [Owner::Room(1)]
        );
        assert_eq!(
            assignment.owners(
                100,
                Some(Affine3A::from_translation(Vec3::new(-25.0, 0.0, 0.0))),
                &rooms,
                &[]
            ),
            [Owner::Room(0)]
        );
        assert_eq!(
            assignment.owners(
                100,
                Some(Affine3A::from_translation(Vec3::new(25.0, 0.0, 0.0))),
                &rooms,
                &[]
            ),
            [Owner::Unbounded]
        );
        assert_eq!(
            automatic(None).owners(100, Some(Affine3A::IDENTITY), &rooms, &[]),
            [Owner::Always]
        );
    }

    #[test]
    fn joined_bounds_are_a_union_without_filling_the_gap() {
        let rooms = [room(1, 0.0), room(2, 25.0)];
        let mut assignment = automatic(Some(ModelSphere {
            center: [25.0, 0.0, 0.0],
            radius: 1.0,
        }));
        assignment.masters = vec![Some(0), Some(0)];
        assignment.joined = vec![vec![0, 1], vec![]];
        assert!(assignment.contains(&rooms, 0, Vec3::new(25.0, 0.0, 0.0)));
        assert!(!assignment.contains(&rooms, 0, Vec3::new(12.5, 0.0, 0.0)));
        assert_eq!(
            assignment.owners(100, Some(Affine3A::IDENTITY), &rooms, &[]),
            [Owner::Room(0)]
        );
    }

    #[test]
    fn admitted_scale_differences_enclose_the_longest_axis() {
        let sphere = ModelSphere {
            center: [0.0; 3],
            radius: 1000.0,
        };
        let transform = Affine3A::from_scale(Vec3::new(1.0, 1.0, 1.000009));
        let world = transformed(sphere, transform).unwrap();
        assert!(
            world.radius
                >= transform
                    .transform_vector3(Vec3::Z * sphere.radius)
                    .length()
        );
        assert_eq!(
            transformed(sphere, Affine3A::IDENTITY).unwrap().radius,
            sphere.radius
        );
    }

    #[test]
    fn large_portal_keeps_its_plane_normal_for_ownership() {
        let portals = [Portal::from_creation(
            11,
            [0.0; 3],
            [0.0; 3],
            [1.0e10; 2],
            [PortalSide::Room(0), PortalSide::Unbounded],
        )];
        for (z, expected) in [(20.0, Owner::Unbounded), (0.0, Owner::Portal(11))] {
            let sphere = ModelSphere {
                center: [0.0, 0.0, z],
                radius: 1.0,
            };
            assert_eq!(
                owner_for_space(PortalSide::Unbounded, sphere, &portals),
                expected
            );
        }
    }

    #[test]
    fn one_portal_is_shared_and_multiple_portals_use_the_common_node() {
        let portal = |id, z: f32| {
            Portal::from_creation(
                id,
                [0.0, -z, 0.0],
                [0.0; 3],
                [10.0, 5.0],
                [PortalSide::Room(0), PortalSide::Unbounded],
            )
        };
        let sphere = ModelSphere {
            center: [0.0; 3],
            radius: 2.0,
        };
        let portals = [portal(11, -0.5), portal(12, 0.5)];
        assert_eq!(
            owner_for_space(PortalSide::Room(0), sphere, &portals[..1]),
            Owner::Portal(11)
        );
        assert_eq!(
            owner_for_space(PortalSide::Room(0), sphere, &portals),
            Owner::Always
        );
        assert_eq!(
            owner_for_space(PortalSide::Unbounded, sphere, &portals[..1]),
            Owner::Portal(11)
        );
        // Multiple XPOD rows of one marker do not become several physical portals.
        assert_eq!(
            owner_for_space(
                PortalSide::Room(0),
                sphere,
                &[portals[0].clone(), portals[0].clone()]
            ),
            Owner::Portal(11)
        );
    }

    #[test]
    fn named_initial_targets_do_not_require_source_spheres() {
        let rooms = [room(1, 0.0), room(2, 25.0)];
        let mut assignment = automatic(None);
        assignment.rules.get_mut(&100).unwrap().rule = Rule::Target(PortalSide::Room(1));
        assert_eq!(
            assignment.initial_owners(100, Affine3A::IDENTITY, &rooms, &[]),
            [Owner::Room(1)]
        );
        assert_eq!(
            assignment.owners(100, Some(Affine3A::IDENTITY), &rooms, &[]),
            [Owner::Always]
        );
    }

    #[test]
    fn authored_targets_override_initial_geometry_and_movement_rechecks_containment() {
        let rooms = [room(1, 0.0), room(2, 25.0)];
        let mut assignment = automatic(Some(ModelSphere {
            center: [0.0; 3],
            radius: 1.0,
        }));
        assignment.rules.get_mut(&100).unwrap().rule = Rule::Target(PortalSide::Room(1));
        assert_eq!(
            assignment.initial_owners(100, Affine3A::IDENTITY, &rooms, &[]),
            [Owner::Room(1)]
        );
        assert_eq!(
            assignment.owners(100, Some(Affine3A::from_translation(Vec3::X)), &rooms, &[]),
            [Owner::Room(0)]
        );
        assert_eq!(
            assignment.owners(100, Some(Affine3A::IDENTITY), &rooms, &[]),
            [Owner::Room(0)],
            "returning to the spawn transform is an update, not another initial attachment"
        );
        assignment.rules.get_mut(&100).unwrap().rule = Rule::None;
        assert_eq!(
            assignment.owners(100, Some(Affine3A::IDENTITY), &rooms, &[]),
            [Owner::Always]
        );
        assignment.rules.get_mut(&100).unwrap().rule = Rule::Target(PortalSide::Unknown);
        assert_eq!(
            assignment.owners(100, Some(Affine3A::IDENTITY), &rooms, &[]),
            [Owner::Always]
        );
    }

    #[test]
    fn overlapping_master_candidates_preserve_possible_native_owners() {
        let rooms = [room(1, 0.0), room(2, 0.0)];
        let assignment = automatic(Some(ModelSphere {
            center: [0.0; 3],
            radius: 1.0,
        }));
        let owners = assignment.owners(100, Some(Affine3A::IDENTITY), &rooms, &[]);
        assert_eq!(owners, [Owner::Room(0), Owner::Room(1)]);
        let hidden = CellVisibility {
            rooms: vec![Default::default(), Default::default()],
            unbounded: Default::default(),
        };
        assert!(!draws(&owners, &[], &hidden, None));
    }

    #[test]
    fn portal_sphere_test_matches_native_instruction_probe() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/portal-sphere-native-1.7.104.json"
        ))
        .unwrap();
        assert_eq!(corpus["tested_cases"], 1617);
        let portal = Portal::from_creation(
            11,
            [0.0; 3],
            [0.0; 3],
            [10.0, 5.0],
            [PortalSide::Room(0), PortalSide::Unbounded],
        );
        for case in corpus["cases"].as_array().unwrap() {
            let sphere: ModelSphere = serde_json::from_value(case.clone()).unwrap();
            assert_eq!(
                sphere_intersects_portal(sphere, &portal),
                case["intersects"].as_bool().unwrap(),
                "{sphere:?}"
            );
        }
    }

    #[test]
    fn source_sphere_assignment_does_not_use_the_old_box_portal_overlap() {
        use bevy::camera::primitives::Aabb;
        let portal = Portal::from_creation(
            11,
            [0.0; 3],
            [0.0; 3],
            [10.0, 5.0],
            [PortalSide::Room(0), PortalSide::Unbounded],
        );
        let sphere = ModelSphere {
            center: [11.0, 0.0, 0.0],
            radius: 1.0,
        };
        let old_box = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::ONE, Vec3::ONE),
            Affine3A::from_translation(Vec3::from_array(sphere.center)),
        )
        .unwrap();
        assert!(old_box.intersects_portal(&portal.corners));
        assert!(!sphere_intersects_portal(sphere, &portal));
        let rotated = Portal::from_creation(
            12,
            [7.0, 9.0, -11.0],
            [0.2, 0.3, 0.7],
            [10.0, 5.0],
            portal.rooms,
        );
        let sphere = ModelSphere {
            center: (rotated.center()
                + (rotated.corners[1] - rotated.corners[0]).normalize() * 11.0)
                .to_array(),
            radius: 1.0,
        };
        assert!(!sphere_intersects_portal(sphere, &rotated));
    }
}
