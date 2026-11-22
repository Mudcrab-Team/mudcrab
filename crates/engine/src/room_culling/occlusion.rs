//! View-dependent shadows of independent authored planes and boxes.

use super::{PortalSide, Room};
use bevy::{
    math::{DVec3, DVec4},
    prelude::*,
};

// Mudcrab uses these tolerances to leave borderline shadow tests visible.
const SHADOW_DISTANCE_MARGIN: f32 = 1.0e-3;
const MIN_ABS_FACING_DOT: f32 = 1.0e-3;
const MIN_EDGE_CROSS_LENGTH: f32 = 1.0e-3;
// Mudcrab camera clearance in Creation units. The retail five-unit comparison at
// RVA 0xff7df0 applies to portal inclusion (docs/research/skyrim-culling-rea-parity.md).
const NEAR_SHAPE_DISTANCE: f32 = 5.0;

#[derive(Debug, Clone)]
pub(crate) struct Occluder {
    pub(super) ref_id: u32,
    pub(super) owner: PortalSide,
    pub(super) center: Vec3,
    rotation: Quat,
    half: Vec3,
    shape: Shape,
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    Plane,
    Box,
}

/// A convex shadow, with normalized inward-facing half-spaces.
#[derive(Clone, Debug)]
pub(super) struct OcclusionVolume {
    planes: Vec<DVec4>,
}

impl OcclusionVolume {
    pub(super) fn intersects_aperture(&self, polygon: &[DVec3]) -> bool {
        super::volume::clip_aperture(polygon, self.planes.iter().copied()).is_some()
    }

    /// Crossing any shadow boundary keeps the sphere visible.
    pub(super) fn contains_sphere(&self, center: Vec3, radius: f32) -> bool {
        let center = center.as_dvec3();
        let radius = f64::from(radius);
        center.is_finite()
            && radius.is_finite()
            && radius > 0.0
            && !self.planes.is_empty()
            && self.planes.iter().all(|plane| {
                let distance = plane.truncate().dot(center) + plane.w;
                // Dot products in large cell coordinates can lose more than a fixed epsilon.
                let rounding = 8.0
                    * f64::from(f32::EPSILON)
                    * (plane.truncate().abs().dot(center.abs()) + plane.w.abs() + radius);
                distance.is_finite()
                    && distance - radius > f64::from(SHADOW_DISTANCE_MARGIN) + rounding
            })
    }
}

fn through_point(unit_normal: DVec3, point: DVec3) -> Option<DVec4> {
    let plane = unit_normal.extend(-unit_normal.dot(point));
    plane.is_finite().then_some(plane)
}

pub(super) struct View {
    camera: DVec3,
    near: DVec4,
    parallel: Option<DVec3>,
}

impl View {
    pub(super) fn new(camera: Vec3, clip: Mat4) -> Option<Self> {
        if !camera.is_finite() || !clip.is_finite() {
            return None;
        }
        let near = clip.row(3).as_dvec4() - clip.row(2).as_dvec4();
        // A long orthographic depth range can give a valid near plane a tiny normal.
        let near = near / near.truncate().length();
        if !near.is_finite() {
            return None;
        }
        let parallel = if clip.row(3).truncate() == Vec3::ZERO {
            Some(super::volume::orthographic_forward(clip)?.as_dvec3())
        } else {
            None
        };
        Some(Self {
            camera: camera.as_dvec3(),
            near,
            parallel,
        })
    }

    fn beyond_near(&self, corners: &[DVec3]) -> bool {
        corners.iter().all(|corner| {
            corner.is_finite()
                && self.near.dot(corner.extend(1.0)) > f64::from(SHADOW_DISTANCE_MARGIN)
        })
    }

    fn edge_plane(&self, a: DVec3, b: DVec3, inside: DVec3) -> Option<DVec4> {
        let edge = b - a;
        let ray = self.parallel.unwrap_or_else(|| a - self.camera);
        // Crossing two nearly parallel corner rays can lose the short edge to cancellation.
        let normal = edge.cross(ray);
        let length = normal.length();
        if !length.is_finite() || length <= f64::from(MIN_EDGE_CROSS_LENGTH) {
            return None;
        }
        let normal = normal / length;
        let distance = normal.dot(inside - a);
        if !distance.is_finite() || distance.abs() <= f64::from(SHADOW_DISTANCE_MARGIN) {
            return None;
        }
        through_point(normal * distance.signum(), a)
    }
}

impl Occluder {
    pub(super) fn program_size(&self) -> (u8, u8) {
        match self.shape {
            Shape::Plane => (2, 1),
            Shape::Box => (6, 2), // OR of two exclusion predicates
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_creation(
        ref_id: u32,
        shape: u32,
        position: [f32; 3],
        rotation: [f32; 3],
        half: [f32; 3],
        scale: f32,
        owner: PortalSide,
    ) -> Option<Self> {
        let shape = match shape {
            1 if half.iter().all(|&value| value > 0.0) => Shape::Box,
            3 if half[0] > 0.0 && half[1] >= 0.0 && half[2] > 0.0 => Shape::Plane,
            _ => return None,
        };
        if !scale.is_finite()
            || scale <= 0.0
            || !position
                .iter()
                .chain(&rotation)
                .chain(&half)
                .all(|value| value.is_finite())
        {
            return None;
        }
        let room = Room::from_creation(ref_id, position, rotation, half);
        // Keep the shadow inside both the authored and placement-scaled shapes.
        let half = room.half * scale.min(1.0);
        if !room.center.is_finite()
            || !room.rotation.is_finite()
            || !room.rotation.is_normalized()
            || !half.is_finite()
        {
            return None;
        }
        Some(Self {
            ref_id,
            owner,
            center: room.center,
            rotation: room.rotation,
            half,
            shape,
        })
    }

    #[cfg(test)]
    fn shadow(&self, camera: Vec3, clip: Mat4) -> Option<OcclusionVolume> {
        self.shadow_in(&View::new(camera, clip)?)
    }

    pub(super) fn shadow_in(&self, view: &View) -> Option<OcclusionVolume> {
        match self.shape {
            Shape::Plane => self.plane_shadow(view),
            Shape::Box => self.box_shadow(view),
        }
    }

    fn plane_shadow(&self, view: &View) -> Option<OcclusionVolume> {
        let center = self.center.as_dvec3();
        let rotation = self.rotation.as_dquat().normalize();
        let x = rotation * DVec3::X * f64::from(self.half.x);
        let y = rotation * DVec3::Y * f64::from(self.half.y);
        let normal = rotation * DVec3::Z;
        let distance = normal.dot(view.camera - center);
        if !distance.is_finite() || distance.abs() <= f64::from(NEAR_SHAPE_DISTANCE) {
            return None;
        }
        let corners = [
            center - x - y,
            center + x - y,
            center + x + y,
            center - x + y,
        ];
        if !view.beyond_near(&corners) {
            return None;
        }
        let hidden_normal = if let Some(forward) = view.parallel {
            let facing = normal.dot(forward);
            if facing.abs() <= f64::from(MIN_ABS_FACING_DOT) {
                return None;
            }
            normal * facing.signum()
        } else {
            -normal * distance.signum()
        };
        let mut planes = Vec::with_capacity(5);
        for index in 0..corners.len() {
            planes.push(view.edge_plane(
                corners[index],
                corners[(index + 1) % corners.len()],
                center,
            )?);
        }
        planes.push(through_point(hidden_normal, center)?);
        Some(OcclusionVolume { planes })
    }

    fn box_shadow(&self, view: &View) -> Option<OcclusionVolume> {
        let center = self.center.as_dvec3();
        let rotation = self.rotation.as_dquat().normalize();
        let half = self.half.as_dvec3();
        let local_camera = rotation.conjugate() * (view.camera - center);
        let gap = (local_camera.abs() - half).max(DVec3::ZERO);
        if !gap.is_finite() || gap.length() <= f64::from(NEAR_SHAPE_DISTANCE) {
            return None;
        }
        let axes = [
            rotation * DVec3::X,
            rotation * DVec3::Y,
            rotation * DVec3::Z,
        ];
        let corners: [DVec3; 8] = std::array::from_fn(|index| {
            center
                + (0..3)
                    .map(|axis| {
                        let sign = if index & (1 << axis) == 0 { -1.0 } else { 1.0 };
                        axes[axis] * half[axis] * sign
                    })
                    .sum::<DVec3>()
        });
        if !view.beyond_near(&corners) {
            return None;
        }
        let mut visible = [false; 6];
        let mut planes = Vec::with_capacity(9);
        for (axis, &direction) in axes.iter().enumerate() {
            for side in 0..2 {
                let sign = if side == 0 { -1.0 } else { 1.0 };
                let outward = direction * sign;
                let point = center + outward * half[axis];
                let facing = if let Some(forward) = view.parallel {
                    -outward.dot(forward)
                } else {
                    outward.dot(view.camera - point)
                };
                if facing > 0.0 {
                    visible[axis * 2 + side] = true;
                    // A ray has entered the opaque box only after crossing every visible face.
                    planes.push(through_point(-outward, point)?);
                }
            }
        }
        if !visible.iter().any(|&face| face) {
            return None;
        }
        if let Some(forward) = view.parallel {
            // Projecting a box along parallel rays gives a strip for each edge direction.
            // Work in box space so a nearly parallel edge needs no subtraction of world corners.
            let local_ray = rotation.conjugate() * forward;
            for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
                if let Some(normal) = axis.cross(local_ray).try_normalize() {
                    let extent = normal.abs().dot(half);
                    let inward = rotation * normal;
                    planes.push(through_point(inward, center - inward * extent)?);
                    planes.push(through_point(-inward, center + inward * extent)?);
                }
            }
            return Some(OcclusionVolume { planes });
        }
        for index in 0..8 {
            for along in 0..3 {
                if index & (1 << along) != 0 {
                    continue;
                }
                let face = |axis: usize| axis * 2 + usize::from(index & (1 << axis) != 0);
                let a = (along + 1) % 3;
                let b = (along + 2) % 3;
                if visible[face(a)] != visible[face(b)] {
                    planes.push(view.edge_plane(
                        corners[index],
                        corners[index | (1 << along)],
                        center,
                    )?);
                }
            }
        }
        (planes.len() >= 5).then_some(OcclusionVolume { planes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perspective(camera: Vec3, target: Vec3) -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.1)
            * Mat4::look_at_rh(camera, target, Vec3::Y)
    }

    fn occluder(shape: u32, center: Vec3, half: Vec3) -> Occluder {
        Occluder::from_creation(
            1,
            shape,
            [center.x, -center.z, center.y],
            [0.0; 3],
            [half.x, half.z, half.y],
            1.0,
            PortalSide::Unbounded,
        )
        .unwrap()
    }

    #[test]
    fn a_plane_hides_only_spheres_wholly_in_its_shadow() {
        let camera = Vec3::ZERO;
        let shape = occluder(3, Vec3::new(0.0, 0.0, -20.0), Vec3::new(5.0, 5.0, 0.0));
        let shadow = shape.shadow(camera, perspective(camera, -Vec3::Z)).unwrap();
        assert!(shadow.contains_sphere(Vec3::new(0.0, 0.0, -40.0), 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -10.0), 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(15.0, 0.0, -40.0), 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(10.0, 0.0, -40.0), 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -21.0), 2.0));
    }

    #[test]
    fn a_distant_off_axis_plane_keeps_spheres_outside_its_shadow() {
        let camera = Vec3::ZERO;
        let center = Vec3::new(7568.0, 5072.0, -16000.0);
        let shape = occluder(3, center, Vec3::new(3.2, 3.2, 0.0));
        let shadow = shape.shadow(camera, perspective(camera, -Vec3::Z)).unwrap();

        assert!(shadow.contains_sphere(center * 2.0, 0.1));
        assert!(
            !shadow.contains_sphere(Vec3::new(15136.0, 10150.8, -32000.0), 0.1),
            "the center ray hits y=5075.4, beyond the edge at y=5075.2",
        );
    }

    #[test]
    fn translated_orthographic_shadow_keeps_the_authored_aperture_edge() {
        let eye = Vec3::X * 65536.0;
        let clip = Mat4::orthographic_rh(-100.0, 100.0, -100.0, 100.0, 100.0, 0.1)
            * Mat4::from_translation(-eye);
        let shadow = occluder(3, eye - Vec3::Z * 20.0, Vec3::new(1.004, 2.0, 0.0))
            .shadow(eye, clip)
            .unwrap();
        for (start, overlaps) in [(eye.x + 1.0078125, false), (eye.x + 1.0, true)] {
            let corners = [(0., -1.), (1., -1.), (1., 1.), (0., 1.)]
                .map(|(x, y)| Vec3::new(start + x, y, -40.0));
            assert_eq!(
                shadow.intersects_aperture(&corners.map(|p| p.as_dvec3())),
                overlaps
            );
        }
    }
    fn check_translated_shadow_edge(shape: Occluder) {
        let camera = Vec3::new(65536.0, 0.0, 0.0);
        let shadow = shape
            .shadow(camera, perspective(camera, camera - Vec3::Z))
            .unwrap();
        assert!(shadow.contains_sphere(Vec3::new(65536.0, 0.0, -10240.0), 0.1));
        assert!(!shadow.contains_sphere(Vec3::new(66100.0, 0.0, -10240.0), 0.1));
        assert!(
            !shadow.contains_sphere(Vec3::new(66051.0, 0.0, -10240.0), 0.1),
            "at z=-20 the ray crosses x=65536+1.00586, beyond the 1.004 half-extent",
        );
    }

    #[test]
    fn a_translated_plane_keeps_spheres_just_outside_its_shadow() {
        check_translated_shadow_edge(occluder(
            3,
            Vec3::new(65536.0, 0.0, -20.0),
            Vec3::new(1.004, 2.0, 0.0),
        ));
    }

    #[test]
    fn a_translated_box_keeps_spheres_just_outside_its_shadow() {
        check_translated_shadow_edge(occluder(
            1,
            Vec3::new(65536.0, 0.0, -21.0),
            Vec3::new(1.004, 2.0, 1.0),
        ));
    }

    #[test]
    fn plane_occlusion_works_from_either_side() {
        let shape = occluder(3, Vec3::new(0.0, 0.0, -20.0), Vec3::new(5.0, 5.0, 0.0));
        let camera = Vec3::new(0.0, 0.0, -40.0);
        let shadow = shape
            .shadow(camera, perspective(camera, Vec3::ZERO))
            .unwrap();
        assert!(shadow.contains_sphere(Vec3::new(0.0, 0.0, -10.0), 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -30.0), 1.0));
    }

    #[test]
    fn a_box_hides_geometry_behind_it_and_keeps_foreground_and_edges() {
        let camera = Vec3::ZERO;
        let shape = occluder(1, Vec3::new(0.0, 0.0, -20.0), Vec3::splat(5.0));
        let shadow = shape.shadow(camera, perspective(camera, -Vec3::Z)).unwrap();
        assert!(shadow.contains_sphere(Vec3::new(0.0, 0.0, -40.0), 1.0));
        assert!(shadow.contains_sphere(shape.center, 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -10.0), 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(20.0, 0.0, -40.0), 1.0));
        assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -15.0), 1.0));
    }

    #[test]
    fn a_diagonal_box_requires_crossing_all_visible_face_caps() {
        let camera = Vec3::new(-20.0, 0.0, 0.0);
        let shape = occluder(1, Vec3::new(0.0, 0.0, -20.0), Vec3::splat(5.0));
        let shadow = shape
            .shadow(camera, perspective(camera, shape.center))
            .unwrap();
        assert!(!shadow.contains_sphere(Vec3::new(-4.0, 0.0, -14.0), 0.1));
        assert!(shadow.contains_sphere(Vec3::new(-4.0, 0.0, -16.0), 0.1));
    }

    #[test]
    fn a_rotated_scaled_box_uses_its_authored_frame() {
        let shape = Occluder::from_creation(
            7,
            1,
            [0.0, 20.0, 0.0],
            [0.1, 0.2, 0.7],
            [3.0, 5.0, 4.0],
            2.0,
            PortalSide::Room(0),
        )
        .unwrap();
        let offset = shape.rotation * Vec3::new(30.0, 25.0, 40.0);
        let camera = shape.center + offset;
        let shadow = shape
            .shadow(camera, perspective(camera, shape.center))
            .unwrap();
        assert!(shadow.contains_sphere(shape.center - offset, 0.5));
        assert!(!shadow.contains_sphere(camera - offset * 0.2, 0.5));
        assert_eq!(shape.ref_id, 7);
        assert_eq!(shape.owner, PortalSide::Room(0));
    }

    #[test]
    fn orthographic_shadows_keep_parallel_sides() {
        for far in [100.0, 10000.0] {
            let clip = Mat4::orthographic_rh(-100.0, 100.0, -100.0, 100.0, far, 0.1);
            let depth = -0.7 * far;
            for kind in [1, 3] {
                let shape = occluder(kind, Vec3::new(0.0, 0.0, -20.0), Vec3::splat(5.0));
                let shadow = shape.shadow(Vec3::ZERO, clip).unwrap();
                assert!(shadow.contains_sphere(Vec3::new(3.0, 0.0, depth), 1.0));
                assert!(!shadow.contains_sphere(Vec3::new(6.0, 0.0, depth), 0.1));
                assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -10.0), 1.0));
            }
        }
    }

    #[test]
    fn shallow_orthographic_rays_keep_spheres_beside_a_long_box_visible() {
        let projection = Mat4::orthographic_rh(-100.0, 100.0, -100.0, 100.0, 1000.0, 0.1);
        let clip = projection * Mat4::look_at_rh(Vec3::ZERO, Vec3::new(0.0005, 0.0, -1.0), Vec3::Y);
        let shape = occluder(1, Vec3::new(0.0, 0.0, -110.0), Vec3::new(0.01, 5.0, 100.0));
        assert!(
            !shape
                .shadow(Vec3::ZERO, clip)
                .is_some_and(|shadow| shadow.contains_sphere(Vec3::new(-0.1, 0.0, -110.0), 0.01))
        );
        assert!(
            shape
                .shadow(Vec3::ZERO, projection)
                .unwrap()
                .contains_sphere(Vec3::new(0.0, 0.0, -220.0), 0.002)
        );
    }

    #[test]
    fn orthographic_shadows_follow_rays_through_an_affine_cell_root() {
        let world_from_local = Mat4::from_scale(Vec3::new(2.0, 1.0, 1.0));
        let view = Mat4::look_at_rh(Vec3::ZERO, Vec3::new(1.0, 0.0, -1.0), Vec3::Y);
        let clip = Mat4::orthographic_rh(-100.0, 100.0, -100.0, 100.0, 1000.0, 0.1)
            * view
            * world_from_local;
        let shape = occluder(3, Vec3::new(0.0, 0.0, -20.0), Vec3::splat(8.0));
        let shadow = shape.shadow(Vec3::ZERO, clip).unwrap();
        assert!(
            !shadow.contains_sphere(Vec3::new(20.0, 0.0, -30.0), 0.1),
            "the actual local ray hits x=15, outside the eight-unit rectangle"
        );
        assert!(
            shadow.contains_sphere(Vec3::new(5.0, 0.0, -30.0), 0.1),
            "the actual local ray hits the center of the rectangle"
        );
    }

    #[test]
    fn nearby_shapes_and_shapes_crossing_the_near_plane_do_not_occlude() {
        let clip = perspective(Vec3::ZERO, -Vec3::Z);
        for kind in [1, 3] {
            let shape = occluder(kind, Vec3::new(0.0, 0.0, -4.0), Vec3::splat(1.0));
            assert!(shape.shadow(Vec3::ZERO, clip).is_none());
        }
        let shape = occluder(1, Vec3::new(0.0, 0.0, -20.0), Vec3::splat(5.0));
        assert!(
            shape
                .shadow(
                    shape.center,
                    perspective(shape.center, shape.center - Vec3::Z)
                )
                .is_none()
        );
        let clip = Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 1.0, 20.0);
        assert!(shape.shadow(Vec3::ZERO, clip).is_none());
        assert!(shape.shadow(Vec3::ZERO, Mat4::ZERO).is_none());
    }

    #[test]
    fn box_shadows_agree_with_independent_segment_box_intersections() {
        // Slab intersection is independent of the silhouette/cap construction above.
        fn segment_hits_box(from: Vec3, to: Vec3, half: Vec3) -> bool {
            let delta = to - from;
            let mut enter = 0.0_f32;
            let mut leave = 1.0_f32;
            for axis in 0..3 {
                if delta[axis].abs() < 1.0e-6 {
                    if from[axis].abs() > half[axis] {
                        return false;
                    }
                } else {
                    let a = (-half[axis] - from[axis]) / delta[axis];
                    let b = (half[axis] - from[axis]) / delta[axis];
                    enter = enter.max(a.min(b));
                    leave = leave.min(a.max(b));
                    if enter > leave {
                        return false;
                    }
                }
            }
            enter <= leave
        }

        let shape = Occluder::from_creation(
            1,
            1,
            [1000.0, 2000.0, -3000.0],
            [0.2, -0.4, 0.8],
            [3.0, 5.0, 4.0],
            1.5,
            PortalSide::Unbounded,
        )
        .unwrap();
        let from_local = |point: Vec3| shape.center + shape.rotation * point;
        let cameras = [
            Vec3::new(-30.0, 0.0, 0.0),
            Vec3::new(30.0, 0.0, 0.0),
            Vec3::new(0.0, -30.0, 0.0),
            Vec3::new(0.0, 30.0, 0.0),
            Vec3::new(0.0, 0.0, -30.0),
            Vec3::new(0.0, 0.0, 30.0),
            Vec3::new(-30.0, 20.0, 0.0),
            Vec3::new(-30.0, 20.0, 30.0),
            Vec3::new(30.0, -20.0, -30.0),
        ];
        let mut hidden = 0;
        for local_camera in cameras {
            let camera = from_local(local_camera);
            let up = shape.rotation
                * if local_camera.normalize().dot(Vec3::Y).abs() > 0.9 {
                    Vec3::Z
                } else {
                    Vec3::Y
                };
            let view = Mat4::look_at_rh(camera, shape.center, up);
            for parallel in [false, true] {
                let projection = if parallel {
                    Mat4::orthographic_rh(-100.0, 100.0, -100.0, 100.0, 1000.0, 0.1)
                } else {
                    Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.1)
                };
                let shadow = shape.shadow(camera, projection * view).unwrap();
                for x in [-30.0, -15.0, -6.0, 0.0, 6.0, 15.0, 30.0] {
                    for y in [-30.0, -15.0, -6.0, 0.0, 6.0, 15.0, 30.0] {
                        for z in [-30.0, -15.0, -6.0, 0.0, 6.0, 15.0, 30.0] {
                            let point = Vec3::new(x, y, z);
                            if shadow.contains_sphere(from_local(point), 0.05) {
                                let from = if parallel {
                                    point + local_camera.normalize() * 1000.0
                                } else {
                                    local_camera
                                };
                                assert!(
                                    segment_hits_box(from, point, shape.half),
                                    "camera={local_camera:?}, point={point:?}, parallel={parallel}"
                                );
                                hidden += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(hidden > 100, "the oracle check must exercise real shadows");
    }

    #[test]
    fn malformed_geometry_and_bounds_keep_geometry_visible() {
        assert!(
            Occluder::from_creation(
                0,
                2,
                [0.0; 3],
                [0.0; 3],
                [1.0; 3],
                1.0,
                PortalSide::Unbounded
            )
            .is_none()
        );
        assert!(
            Occluder::from_creation(
                0,
                1,
                [0.0; 3],
                [0.0; 3],
                [-1.0; 3],
                1.0,
                PortalSide::Unbounded
            )
            .is_none()
        );
        assert!(
            Occluder::from_creation(
                0,
                1,
                [0.0; 3],
                [0.0; 3],
                [1.0; 3],
                0.0,
                PortalSide::Unbounded
            )
            .is_none()
        );
        assert!(
            Occluder::from_creation(
                0,
                3,
                [f32::NAN; 3],
                [0.0; 3],
                [1.0; 3],
                1.0,
                PortalSide::Unbounded
            )
            .is_none()
        );
        let shape = occluder(3, Vec3::new(0.0, 0.0, -20.0), Vec3::new(5.0, 5.0, 0.0));
        let shadow = shape
            .shadow(Vec3::ZERO, perspective(Vec3::ZERO, -Vec3::Z))
            .unwrap();
        assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -40.0), f32::NAN));
        assert!(!shadow.contains_sphere(Vec3::new(0.0, 0.0, -40.0), 0.0));
        assert!(!shadow.contains_sphere(Vec3::splat(f32::INFINITY), 1.0));
    }
}
