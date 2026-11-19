//! Path-local inclusion and exclusion volumes. Separate portal paths form a union.

use bevy::{camera::primitives::Aabb, math::Affine3A, prelude::*};
use std::sync::Arc;

use super::occlusion::OcclusionVolume;

// These are conservative work limits, not Skyrim's operator budget. Overflow widens the room
// to the camera frustum with local exclusions and propagates that wider volume.
const BASE_ROOM_CONTRIBUTIONS: usize = 16;
const MAX_VOLUME_PLANES: usize = 64;
const PLANE_EPSILON: f32 = 1.0e-3;

/// A mesh's transformed AABB, including shear and negative or nonuniform scale.
pub(super) struct MeshBounds {
    center: Vec3,
    half_axes: [Vec3; 3],
}

impl MeshBounds {
    /// Enclose all affine-transformed AABB corners, including shear. Exclusions need full
    /// containment, so a conservative sphere can only retain extra geometry.
    fn sphere_radius(&self) -> Option<f32> {
        let radius = (0..8)
            .map(|corner| {
                self.half_axes
                    .iter()
                    .enumerate()
                    .map(|(axis, half)| {
                        if corner & (1 << axis) == 0 {
                            -*half
                        } else {
                            *half
                        }
                    })
                    .sum::<Vec3>()
                    .length()
            })
            .fold(0.0_f32, f32::max);
        (radius.is_finite() && radius > 0.0).then_some(radius)
    }

    fn radius(&self, axis: Vec3) -> f32 {
        self.half_axes
            .iter()
            .map(|half| axis.dot(*half).abs())
            .sum()
    }

    /// Conservative overlap of the transformed bounds and a portal rectangle. False overlaps
    /// only add endpoint visibility; they cannot remove a genuinely shared reference.
    pub(super) fn intersects_portal(&self, corners: &[Vec3; 4]) -> bool {
        let center = corners.iter().copied().sum::<Vec3>() * 0.25;
        let u = (corners[1] - corners[0]) * 0.5;
        let v = (corners[3] - corners[0]) * 0.5;
        [u, v, u.cross(v)].iter().all(|axis| {
            let axis = axis.normalize_or_zero();
            let distance = axis.dot(self.center - center).abs();
            let radius = self.radius(axis) + axis.dot(u).abs() + axis.dot(v).abs();
            !distance.is_finite() || !radius.is_finite() || distance <= radius + PLANE_EPSILON
        })
    }
    pub(super) fn transformed(bounds: &Aabb, local_from_mesh: Affine3A) -> Option<Self> {
        let center = local_from_mesh.transform_point3(bounds.center.into());
        let half_axes = [
            Vec3::from(local_from_mesh.matrix3.x_axis * bounds.half_extents.x),
            Vec3::from(local_from_mesh.matrix3.y_axis * bounds.half_extents.y),
            Vec3::from(local_from_mesh.matrix3.z_axis * bounds.half_extents.z),
        ];
        (bounds.half_extents.cmpge(Vec3::ZERO.into()).all()
            && center.is_finite()
            && half_axes.iter().all(|axis| axis.is_finite()))
        .then_some(Self { center, half_axes })
    }
}

/// Intersection of normalized half-spaces: `plane.xyz.dot(point) + plane.w >= 0`.
#[derive(Clone, Debug)]
pub(super) struct VisibilityVolume {
    planes: Vec<Vec4>,
    exclusions: Vec<(u32, Arc<OcclusionVolume>)>,
}

fn normalized(plane: Vec4) -> Option<Vec4> {
    let length = plane.truncate().length();
    let result = plane / length;
    (length > 0.0 && result.is_finite()).then_some(result)
}

/// Decreasing orthographic clip depth follows the viewing rays. An affine cell root can make
/// this direction differ from the near-plane normal, which transforms as a covector.
pub(super) fn orthographic_forward(clip: Mat4) -> Option<Vec3> {
    let determinant = clip.determinant();
    if !determinant.is_finite() || determinant == 0.0 {
        return None;
    }
    (-clip.inverse().z_axis.truncate()).try_normalize()
}

fn clip_aperture(corners: &[Vec3; 4], planes: impl Iterator<Item = Vec4>) -> Option<Vec<Vec3>> {
    let mut polygon = corners.to_vec();
    for plane in planes {
        let mut clipped = Vec::with_capacity(polygon.len() + 1);
        for (index, &a) in polygon.iter().enumerate() {
            let b = polygon[(index + 1) % polygon.len()];
            let da = plane.truncate().dot(a) + plane.w + PLANE_EPSILON;
            let db = plane.truncate().dot(b) + plane.w + PLANE_EPSILON;
            if da >= 0.0 {
                clipped.push(a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                clipped.push(a + (b - a) * (da / (da - db)));
            }
        }
        polygon = clipped;
        if polygon.is_empty() {
            return None;
        }
    }
    Some(polygon)
}

impl VisibilityVolume {
    pub(super) fn camera(clip_from_local: Mat4) -> Option<Self> {
        let (x, y, z, w) = (
            clip_from_local.row(0),
            clip_from_local.row(1),
            clip_from_local.row(2),
            clip_from_local.row(3),
        );
        // Bevy uses reverse Z: w-z is the near plane. Its infinite far plane is omitted.
        let planes: Option<Vec<_>> = [w + x, w - x, w + y, w - y, w - z]
            .into_iter()
            .map(normalized)
            .collect();
        planes.map(|planes| Self {
            planes,
            exclusions: Vec::new(),
        })
    }

    pub(super) fn has_exclusions(&self) -> bool {
        !self.exclusions.is_empty()
    }

    /// Occluders belong to this contributing path, not to the union of every room path.
    pub(super) fn with_occluders(&self, shadows: &[(u32, Arc<OcclusionVolume>)]) -> Self {
        let mut volume = self.clone();
        for (id, shadow) in shadows {
            if !volume.exclusions.iter().any(|(existing, _)| existing == id) {
                volume.exclusions.push((*id, Arc::clone(shadow)));
            }
        }
        volume
    }

    pub(super) fn intersects(&self, bounds: &MeshBounds) -> bool {
        let included = self.planes.iter().all(|plane| {
            let normal = plane.truncate();
            let distance = normal.dot(bounds.center) + plane.w;
            let radius = bounds.radius(normal);
            !distance.is_finite() || !radius.is_finite() || distance + radius >= -PLANE_EPSILON
        });
        included
            && (self.exclusions.is_empty()
                || bounds.sphere_radius().is_none_or(|radius| {
                    !self
                        .exclusions
                        .iter()
                        .any(|(_, shadow)| shadow.contains_sphere(bounds.center, radius))
                }))
    }

    /// A sufficient containment test. Retaining the incoming planes makes cycles redundant.
    fn contains(&self, other: &Self) -> bool {
        self.planes.iter().all(|plane| other.planes.contains(plane))
            && self
                .exclusions
                .iter()
                .all(|(id, _)| other.exclusions.iter().any(|(other_id, _)| other_id == id))
    }

    pub(super) fn through_portal(
        &self,
        corners: &[Vec3; 4],
        camera: Vec3,
        clip: Mat4,
    ) -> Option<Self> {
        self.clip_portal(corners, camera, clip, None)
    }

    pub(super) fn through_authored_portal(
        &self,
        portal: &super::Portal,
        view: &super::PortalView,
    ) -> Option<Self> {
        self.clip_portal(&portal.corners, view.eye, view.clip, Some((portal, view)))
    }

    fn clip_portal(
        &self,
        corners: &[Vec3; 4],
        camera: Vec3,
        clip: Mat4,
        authored: Option<(&super::Portal, &super::PortalView)>,
    ) -> Option<Self> {
        if !corners.iter().all(|point| point.is_finite()) {
            return Some(self.clone());
        }
        let near = normalized(clip.row(3) - clip.row(2));
        // An orthographic eye plane is parallel to the near plane even under cell-root shear.
        // The render-near plane remains part of the mesh volume.
        let eye_plane = near
            .filter(|_| clip.row(3).truncate() == Vec3::ZERO)
            .map(|plane| plane.truncate().extend(-plane.truncate().dot(camera)));
        let ordinary = self
            .planes
            .iter()
            .copied()
            .filter(|plane| Some(*plane) != near)
            .chain(eye_plane);
        let polygon = clip_aperture(corners, ordinary).or_else(|| {
            let (portal, view) = authored?;
            if clip.row(3).truncate() == Vec3::ZERO || !portal.admits_projected_eye(view) {
                return None;
            }
            // The ordinary-view exception still has to pass each inherited aperture.
            clip_aperture(corners, self.planes[5..].iter().copied())
        })?;
        // When the complete aperture projects in front of the eye, keep its actual edges.
        // Baking screen edges into the next aperture would override the native admission above.
        let polygon = if authored.is_some()
            && clip.row(3).truncate() != Vec3::ZERO
            && corners
                .iter()
                .all(|p| clip.row(3).dot(p.extend(1.0)) > PLANE_EPSILON)
        {
            corners.as_slice()
        } else {
            &polygon
        };
        if polygon.len() < 3 || !polygon.iter().all(|point| point.is_finite()) {
            return Some(self.clone());
        }
        let center = polygon.iter().copied().sum::<Vec3>() / polygon.len() as f32;
        let mut through = self.clone();
        for (index, &a) in polygon.iter().enumerate() {
            let b = polygon[(index + 1) % polygon.len()];
            let a = clip * a.extend(1.0);
            let b = clip * b.extend(1.0);
            let edge = Vec3::new(a.x, a.y, a.w).cross(Vec3::new(b.x, b.y, b.w));
            // Lift the homogeneous screen edge back to a plane. This also handles the parallel
            // rays of an orthographic view without constructing a cone from its camera position.
            if let Some(mut plane) =
                normalized(clip.transpose() * Vec4::new(edge.x, edge.y, 0.0, edge.z))
            {
                if plane.dot(center.extend(1.0)) < 0.0 {
                    plane = -plane;
                }
                through.planes.push(plane);
            }
        }
        // Objects before the aperture do not belong to the volume behind it.
        let mut normal = (corners[1] - corners[0])
            .cross(corners[3] - corners[0])
            .normalize_or_zero();
        let forward = if clip.row(3).truncate() == Vec3::ZERO {
            let Some(forward) = orthographic_forward(clip) else {
                return Some(self.clone());
            };
            forward
        } else {
            center - camera
        };
        if normal.dot(forward) < 0.0 {
            normal = -normal;
        }
        if normal.length_squared() > 0.0 {
            through.planes.push(normal.extend(-normal.dot(corners[0])));
        }
        Some(through)
    }
}

/// Union of the volumes arriving at one room. An empty union is unreached.
pub(super) struct RoomVisibility {
    volumes: Vec<VisibilityVolume>,
    contributions: usize,
    contribution_limit: usize,
}

impl Default for RoomVisibility {
    fn default() -> Self {
        Self::for_authored_portals(0)
    }
}

impl RoomVisibility {
    /// Allow one contribution per physical portal before widening combinations of paths.
    pub(super) fn for_authored_portals(portals: usize) -> Self {
        Self {
            volumes: Vec::new(),
            contributions: 0,
            contribution_limit: BASE_ROOM_CONTRIBUTIONS.max(portals),
        }
    }

    pub(super) fn seed(&mut self, volume: VisibilityVolume) {
        self.volumes.clear();
        self.volumes.push(volume);
        self.contributions = 1;
    }

    pub(super) fn volumes(&self) -> &[VisibilityVolume] {
        &self.volumes
    }

    /// Returns only the new contribution that must be propagated to neighbours.
    pub(super) fn insert(
        &mut self,
        mut arriving: VisibilityVolume,
        fallback: &VisibilityVolume,
    ) -> Option<&VisibilityVolume> {
        if self.volumes.iter().any(|old| old.contains(&arriving)) {
            return None;
        }
        if self.contributions >= self.contribution_limit
            || arriving.planes.len() > MAX_VOLUME_PLANES
        {
            arriving = fallback.clone();
        }
        self.contributions += 1;
        self.volumes.retain(|old| !arriving.contains(old));
        self.volumes.push(arriving);
        self.volumes.last()
    }

    pub(super) fn is_reached(&self) -> bool {
        !self.volumes.is_empty()
    }

    pub(super) fn intersects(&self, bounds: Option<&MeshBounds>) -> bool {
        self.is_reached()
            && bounds
                .is_none_or(|bounds| self.volumes.iter().any(|volume| volume.intersects(bounds)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_3, 1.0, 0.1)
    }

    fn camera() -> VisibilityVolume {
        VisibilityVolume::camera(clip()).unwrap()
    }

    fn portal(center: Vec3, rotation: Quat) -> [Vec3; 4] {
        [
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
        ]
        .map(|point| center + rotation * point)
    }

    fn bounds(center: Vec3, half: Vec3) -> MeshBounds {
        MeshBounds::transformed(
            &Aabb::from_min_max(-half, half),
            Affine3A::from_translation(center),
        )
        .unwrap()
    }

    #[test]
    fn portals_before_the_near_plane_still_admit_visible_geometry() {
        for clip in [
            Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_3, 1.0, 16.0),
            Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 100.0, 16.0),
        ] {
            let camera = VisibilityVolume::camera(clip).unwrap();
            let visible = camera
                .through_portal(
                    &portal(Vec3::new(0.0, 0.0, -12.0), Quat::IDENTITY),
                    Vec3::ZERO,
                    clip,
                )
                .expect("a portal in front of the viewpoint must survive traversal clipping");
            assert!(visible.intersects(&bounds(Vec3::new(0.0, 0.0, -24.0), Vec3::splat(0.1))));
            assert!(
                !visible.intersects(&bounds(Vec3::new(0.0, 0.0, -14.0), Vec3::splat(0.1))),
                "mesh testing still applies the near plane"
            );
        }
    }

    #[test]
    fn orthographic_apertures_are_clipped_at_the_eye() {
        let eye = Vec3::new(3.0, 0.0, 20.0);
        let clip = Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 100.0, 16.0)
            * Mat4::from_translation(-eye);
        let camera = VisibilityVolume::camera(clip).unwrap();
        assert!(
            camera
                .through_portal(&portal(eye + Vec3::Z * 20.0, Quat::IDENTITY), eye, clip)
                .is_none()
        );

        let tilted = portal(eye, Quat::from_rotation_y(std::f32::consts::FRAC_PI_4));
        let through = camera.through_portal(&tilted, eye, clip).unwrap();
        assert!(through.intersects(&bounds(eye + Vec3::new(0.5, 0.0, -30.0), Vec3::splat(0.01))));
        assert!(!through.intersects(&bounds(
            eye + Vec3::new(-0.5, 0.0, -30.0),
            Vec3::splat(0.01)
        )));
    }

    #[test]
    fn alternative_paths_do_not_share_their_occluders() {
        let full = camera();
        let shape = super::super::occlusion::Occluder::from_creation(
            10,
            3,
            [0.0, 10.0, 0.0],
            [0.0; 3],
            [4.0, 0.0, 4.0],
            1.0,
            super::super::PortalSide::Unbounded,
        )
        .unwrap();
        let occluded =
            full.with_occluders(&[(10, Arc::new(shape.shadow(Vec3::ZERO, clip()).unwrap()))]);
        let mesh = bounds(Vec3::new(0.0, 0.0, -30.0), Vec3::splat(0.5));
        assert!(!occluded.intersects(&mesh));
        let mut visible = RoomVisibility::default();
        visible.insert(occluded.clone(), &occluded);
        assert!(!visible.intersects(Some(&mesh)));
        assert!(visible.intersects(None), "missing mesh bounds fail open");
        visible.insert(full.clone(), &full);
        assert!(
            visible.intersects(Some(&mesh)),
            "an independent clear path retains the mesh"
        );
        assert!(visible.insert(occluded.clone(), &occluded).is_none());

        let mut limited = RoomVisibility {
            contributions: BASE_ROOM_CONTRIBUTIONS,
            ..Default::default()
        };
        limited.insert(occluded.clone(), &occluded);
        assert!(
            !limited.intersects(Some(&mesh)),
            "budget widening retains local exclusions"
        );
        let zero = bounds(Vec3::new(0.0, 0.0, -30.0), Vec3::ZERO);
        assert!(occluded.intersects(&zero), "degenerate bounds fail open");
    }

    #[test]
    fn aperture_tests_bounds_and_the_space_behind_the_doorway() {
        let volume = camera()
            .through_portal(
                &portal(Vec3::new(0.0, 0.0, -10.0), Quat::IDENTITY),
                Vec3::ZERO,
                clip(),
            )
            .unwrap();
        assert!(volume.intersects(&bounds(Vec3::new(0.0, 0.0, -20.0), Vec3::splat(0.1))));
        assert!(!volume.intersects(&bounds(Vec3::new(4.0, 0.0, -20.0), Vec3::splat(0.1))));
        assert!(!volume.intersects(&bounds(Vec3::new(0.0, 0.0, -5.0), Vec3::splat(0.1))));
        assert!(
            volume.intersects(&bounds(
                Vec3::new(6.0, 0.0, -20.0),
                Vec3::new(4.0, 0.1, 0.1)
            )),
            "a touching bound stays even with its center outside"
        );
        assert!(
            volume
                .through_portal(
                    &portal(Vec3::new(0.0, 0.0, -5.0), Quat::IDENTITY),
                    Vec3::ZERO,
                    clip(),
                )
                .is_none(),
            "a later portal cannot precede the first aperture"
        );
    }

    #[test]
    fn a_rotated_aperture_keeps_its_shape() {
        let volume = camera()
            .through_portal(
                &portal(
                    Vec3::new(0.0, 0.0, -10.0),
                    Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
                ),
                Vec3::ZERO,
                clip(),
            )
            .unwrap();
        assert!(volume.intersects(&bounds(Vec3::new(2.3, 0.0, -20.0), Vec3::splat(0.1))));
        assert!(
            !volume.intersects(&bounds(Vec3::new(2.3, 2.3, -20.0), Vec3::splat(0.1))),
            "the corner of the aperture's screen rectangle is outside its diamond"
        );
    }

    #[test]
    fn an_orthographic_aperture_uses_parallel_rays() {
        let clip = Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 100.0, 0.1);
        let volume = VisibilityVolume::camera(clip)
            .unwrap()
            .through_portal(
                &portal(Vec3::new(0.0, 0.0, -10.0), Quat::IDENTITY),
                Vec3::ZERO,
                clip,
            )
            .unwrap();
        assert!(volume.intersects(&bounds(Vec3::new(0.5, 0.0, -50.0), Vec3::splat(0.1))));
        assert!(
            !volume.intersects(&bounds(Vec3::new(3.0, 0.0, -50.0), Vec3::splat(0.1))),
            "an orthographic aperture does not widen with distance"
        );
    }

    #[test]
    fn an_orthographic_portal_cap_follows_rays_through_an_affine_cell_root() {
        let clip = Mat4::orthographic_rh(-100.0, 100.0, -100.0, 100.0, 1000.0, 0.1)
            * Mat4::look_at_rh(Vec3::ZERO, Vec3::new(1.0, 0.0, -1.0), Vec3::Y)
            * Mat4::from_scale(Vec3::new(2.0, 1.0, 1.0));
        let volume = VisibilityVolume::camera(clip)
            .unwrap()
            .through_portal(
                &portal(
                    Vec3::new(10.0, 0.0, -20.0),
                    Quat::from_rotation_y(std::f32::consts::FRAC_PI_4),
                ),
                Vec3::ZERO,
                clip,
            )
            .unwrap();
        assert!(volume.intersects(&bounds(Vec3::new(15.0, 0.0, -30.0), Vec3::splat(0.1))));
        assert!(!volume.intersects(&bounds(Vec3::new(5.0, 0.0, -10.0), Vec3::splat(0.1))));
    }

    #[test]
    fn separate_paths_do_not_fill_the_gap_between_them() {
        let camera = camera();
        let mut room = RoomVisibility::default();
        for x in [-3.0, 3.0] {
            room.insert(
                camera
                    .through_portal(
                        &portal(Vec3::new(x, 0.0, -10.0), Quat::IDENTITY),
                        Vec3::ZERO,
                        clip(),
                    )
                    .unwrap(),
                &camera,
            );
        }
        for x in [-6.0, 6.0] {
            assert!(room.intersects(Some(&bounds(Vec3::new(x, 0.0, -20.0), Vec3::splat(0.1)))));
        }
        assert!(!room.intersects(Some(&bounds(Vec3::new(0.0, 0.0, -20.0), Vec3::splat(0.1)))));
    }

    #[test]
    fn rotated_scaled_and_sheared_bounds_are_tested_as_a_whole() {
        let volume = camera()
            .through_portal(
                &portal(Vec3::new(0.0, 0.0, -10.0), Quat::IDENTITY),
                Vec3::ZERO,
                clip(),
            )
            .unwrap();
        let aabb = Aabb::from_min_max(-Vec3::new(0.1, 2.0, 0.1), Vec3::new(0.1, 2.0, 0.1));
        let position = Vec3::new(5.0, 0.0, -20.0);
        let unrotated =
            MeshBounds::transformed(&aabb, Affine3A::from_translation(position)).unwrap();
        assert!(!volume.intersects(&unrotated));
        let rotated = MeshBounds::transformed(
            &aabb,
            Affine3A::from_scale_rotation_translation(
                Vec3::new(-1.0, 2.0, 0.5),
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                position,
            ),
        )
        .unwrap();
        assert!(volume.intersects(&rotated));
        let mut shear = Affine3A::from_translation(position);
        shear.matrix3.y_axis.x = 2.0;
        assert!(volume.intersects(&MeshBounds::transformed(&aabb, shear).unwrap()));
    }

    #[test]
    fn bounds_and_portals_crossing_the_view_plane_remain_conservative() {
        let camera = camera();
        assert!(camera.intersects(&bounds(Vec3::new(0.0, 0.0, 0.1), Vec3::splat(1.0))));
        let tilted = portal(
            Vec3::new(0.0, 0.0, -0.5),
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_4),
        );
        assert!(camera.through_portal(&tilted, Vec3::ZERO, clip()).is_some());
        assert!(
            MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::ONE, Vec3::ONE),
                Affine3A::from_translation(Vec3::splat(f32::NAN))
            )
            .is_none()
        );
        let mut room = RoomVisibility::default();
        assert!(
            !room.intersects(None),
            "unreached rooms retain the old room-level cull"
        );
        room.insert(camera.clone(), &camera);
        assert!(
            room.intersects(None),
            "unavailable bounds do not add an aperture cull"
        );
    }

    #[test]
    fn a_volume_budget_overflow_widens_instead_of_dropping_paths() {
        let camera = camera();
        let mut room = RoomVisibility::default();
        for i in 0..=BASE_ROOM_CONTRIBUTIONS {
            let x = -3.0 + 6.0 * i as f32 / BASE_ROOM_CONTRIBUTIONS as f32;
            room.insert(
                camera
                    .through_portal(
                        &portal(Vec3::new(x, 0.0, -10.0), Quat::IDENTITY),
                        Vec3::ZERO,
                        clip(),
                    )
                    .unwrap(),
                &camera,
            );
        }
        assert!(
            room.intersects(Some(&bounds(Vec3::new(0.0, 8.0, -20.0), Vec3::splat(0.1)))),
            "the overflow restores the camera frustum"
        );
        assert!(
            room.insert(camera.clone(), &camera).is_none(),
            "the widened room terminates cycles"
        );
    }

    #[test]
    fn a_long_portal_chain_widens_when_its_plane_budget_is_exhausted() {
        let camera = camera();
        let mut arriving = camera.clone();
        for step in 0..40 {
            arriving = arriving
                .through_portal(
                    &portal(
                        Vec3::new(0.0, 0.0, -10.0 - 20.0 * step as f32),
                        Quat::IDENTITY,
                    ),
                    Vec3::ZERO,
                    clip(),
                )
                .unwrap();
        }
        let bounds = bounds(Vec3::new(5.0, 0.0, -800.0), Vec3::splat(0.1));
        assert!(!arriving.intersects(&bounds));
        let mut room = RoomVisibility::default();
        room.insert(arriving, &camera);
        assert!(
            room.intersects(Some(&bounds)),
            "a plane-budget overflow keeps extra geometry"
        );
    }
}
