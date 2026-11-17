//! Interior room-and-portal culling for the main camera (upstream issue #188).
//!
//! The converter writes the rooms of an interior cell, the doorways between them and which objects
//! sit in which room (`docs/specs/converters/db-schema.md`, section 15; design
//! `docs/research/interior-room-graph-converter.md`). The tables hold facts; this module is the
//! policy that turns them into "do not draw the rooms the camera cannot see into".
//!
//! # What is drawn
//!
//! For a view, find the rooms that contain the camera and walk from them through the portals. At
//! each portal its polygon is clipped against the arriving convex volume. Aperture planes and
//! the plane behind the doorway narrow the next volume. Each room retains the union of its paths,
//! so a gap between visible doorways stays outside the union. Authored `XLRM` groups share one
//! master room and the union of their bounds. Legacy links pass volumes unchanged.
//! Cycles and complex graphs have bounded work: exceeding a contribution or plane budget widens
//! that room to the camera frustum and propagates the wider volume.
//!
//! Versioned graphs distinguish rooms, authored unbounded endpoints, and unknown endpoints. A
//! camera outside every room keeps full rendering because room-box seams are not verified outside
//! space. Unbounded geometry uses its accumulated volume when viewed from a room through an
//! authored outside portal. Unknown endpoints and incomplete authored graphs draw the whole cell.
//! Legacy graphs also fall back on reached portal-less rooms.
//!
//! Authored assignment uses source NIF spheres, bound-center containment, named overrides, and
//! the native sphere/portal test. One touched portal supplies both endpoint volumes; several
//! touched portals use the common node and remain drawn. Ownership is cached across views and
//! recomputed when the reference transform changes. Uncertain native room-map order retains all
//! possible owners. Automatic assignment and updates keep missing source spheres drawn. Initial
//! named targets follow direct native attachment. Each mesh still tests its own bounds.
//!
//! In legacy graphs, references without stored model bounds stay drawn. Pivot-only membership
//! cannot establish overlap coverage, even when a loaded mesh supplies an AABB.
//! A static object's containing and overlapping rooms supply candidate volumes.

//! An object with one pivot-containing room is hidden when its mesh bounds miss every candidate
//! volume. The mesh's current AABB is transformed into cell-local space with its full affine
//! transform, including rotation, scale, and shear. Missing or invalid bounds and `NoFrustumCulling`
//! skip the aperture test but retain the unreached-space test. Versioned graphs also track
//! references whose pivots are in no room as unbounded; their conservative overlap memberships
//! remain candidates. Multiple containing rooms, unknown references, and legacy unassigned
//! references stay drawn. References whose aggregate model bounds touch an attached portal gain
//! the union of both endpoint volumes. Mesh filtering never inserts duplicate entries.
//!
//! # How an object is hidden, and why shadows keep it
//!
//! Bevy 0.19 has no room culling (and none is planned for 0.20; the visibility API is the same).
//! The supported hook is a system in [`VisibilitySystems::CheckVisibility`] that runs after
//! `check_visibility_cpu_culling` and edits the view's [`VisibleEntities`]
//! (`bevy_camera-0.19.0/src/visibility/mod.rs`: the system clears and refills the list at line 861
//! and is registered in the set at line 532; `bevy_render::view::visibility` then extracts the main
//! view's `RenderVisibleEntities` from it). This module removes entities from the `Mesh3d` class of
//! the camera's list and touches nothing else:
//!
//! - **Shadow views keep their casters.** A directional light's casters live in its own
//!   `CascadesVisibleEntities`, a point light's in its `CubemapVisibleEntities` and a spot light's in
//!   `VisibleMeshEntities` (`bevy_light-0.19.0/src/lib.rs`, `check_dir_light_mesh_visibility` and
//!   `check_point_light_mesh_visibility`). They are computed from the entity's `InheritedVisibility`
//!   and bounds, not from a camera's list, and this module never writes them.
//! - **`Visibility::Hidden` is not used**: it would hide the object from shadow passes and from
//!   every other camera. `ViewVisibility` is left alone for the same reason.
//! - **The light entities are not culled.** Only the `Mesh3d` class is edited, so a point light in
//!   an unreached room still reaches Bevy's light passes. Culling lights is future work.
//! - **Entities with `NoCpuCulling`** are not in any camera's [`VisibleEntities`] (they are decided
//!   on the GPU, `check_visibility_gpu_culling`, line 884), so this CPU-side filter cannot hide
//!   them. The engine uses that marker for empty hierarchy nodes, while rendered meshes normally
//!   retain CPU visibility checks. Filtering meshes explicitly marked `NoCpuCulling` would need
//!   a GPU-side input.
//!
//! # Implicit overlap edges
//!
//! In legacy graphs, touching or overlapping boxes gain edges within [`OVERLAP_TOLERANCE`], unless
//! an authored portal or link already joins the pair. Between two rooms with portals, the edge
//! tests the destination box against the incoming volume and clips to its projected rectangle.
//! An edge to a portal-less room instead triggers the whole-cell fallback above.
//!
//! Authored graphs disable these edges and the portal-less-room heuristic. They use the authored
//! master/slave resolution instead. See `docs/research/skyrim-native-culling.md`.
//!
//! # What is never culled by its room
//!
//! The converter records room membership for every placed record, actors included, but a room is
//! where something *was placed*, not where it is. These are treated as in no room and stay drawn:
//!
//! - a reference carrying [`RoomCullExempt`], inserted for actors, doors and activators.
//!   [`RoomCullLegacyExempt`] protects authored overrides and loose inventory items in legacy
//!   graphs; authored assignment preserves `NONE` and updates supported moving reference roots.
//!   **Future actor spawning hooks in here:** whatever spawns an actor
//!   or an animated door outside `spawn_cell` inserts `RoomCullExempt` on the reference entity;
//! - a mesh whose own entity, or whose reference, has a `RigidBody` that moves
//!   (`Dynamic`, `KinematicPositionBased` or `KinematicVelocityBased`), except an authored
//!   reference root with a known source sphere. Moving child meshes still stay drawn.
//!   `Fixed` bodies, which static collision uses, are culled normally.
//!
//! Streamed actors and doors receive the exemption when their reference root is spawned.
//!
//! # Views
//!
//! The main camera ([`StreamingCamera`]) is culled automatically. Another camera, such as
//! a portal view elsewhere, opts in by carrying [`RoomCullingView`]; every other camera, and every
//! shadow view, is left untouched.
//!
//! # Evidence and limits
//!
//! See `docs/research/skyrim-room-culling.md` for the original-engine evidence and the policies
//! that deliberately draw extra geometry. Image comparisons need real converted interiors;
//! synthetic graph tests alone do not prove rendering parity. The first frame that hides anything
//! logs the removed mesh count.
//!
//! # Coordinates
//!
//! The tables are in Creation units and Creation axes. A room box has the half-extents
//! `(half_x, half_z, half_y)` in runtime axes and the orientation
//! `creation_euler_to_runtime_quaternion(rot)`; a portal is the local X-Z rectangle in Creation axes,
//! which is the local X-Y rectangle in runtime axes. The graph is stored in the cell root's local
//! space and the walk runs there, so a render-origin rebase of the root needs no rebuild.

mod assignment;
mod volume;

use volume::{MeshBounds, RoomVisibility, VisibilityVolume};

use crate::{
    config::EngineConfig,
    world::components::{
        CellRef, ExpectedModelBounds, ExteriorCellGrid, FormId, StreamedCellRoot, StreamingCamera,
    },
};
use bevy::{
    camera::{
        primitives::Aabb,
        visibility::{
            NoFrustumCulling, VisibilitySystems, VisibleEntities, check_visibility_cpu_culling,
        },
    },
    math::Affine3A,
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures_lite::future},
};
use bevy_rapier3d::prelude::RigidBody;
use rusqlite::{Connection, OpenFlags};
use std::{
    any::TypeId,
    collections::{HashMap, HashSet},
    path::PathBuf,
};

/// Marks a placed reference that room culling must never hide, such as an actor or animated door.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RoomCullExempt;

/// Authored policies and loose objects stay exempt until native assignment metadata is available.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RoomCullLegacyExempt;

/// Ownership persists until the reference moves or its cell root is replaced.
#[derive(Default)]
pub struct RoomAssignmentCache(HashMap<(u32, Entity, Entity), assignment::CachedAssignment>);

pub fn assignment_follows_transform(record_type: Option<&str>) -> bool {
    matches!(
        record_type,
        Some(
            "MISC" | "ALCH" | "AMMO" | "ARMO" | "BOOK" | "INGR" | "KEYM" | "SCRL" | "SLGM" | "WEAP"
        )
    )
}

/// Whether a base record type (the `records.record_type` of a reference's base object) is one
/// room culling must not hide: actors and their leveled lists, doors, activators, and the
/// inventory records that become loose, physics-driven objects when placed.
pub fn exempt_record_type(record_type: Option<&str>) -> bool {
    matches!(
        record_type,
        Some(
            "NPC_"
                | "LVLN"
                | "LVLI"
                | "CREA"
                | "DOOR"
                | "ACTI"
                | "MISC"
                | "ALCH"
                | "AMMO"
                | "ARMO"
                | "BOOK"
                | "INGR"
                | "KEYM"
                | "SCRL"
                | "SLGM"
                | "WEAP"
        )
    )
}

/// Opt-in marker for a camera, other than the main one, that culls by room (a portal view).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RoomCullingView;

/// A doorway closer than this to the camera counts as fully open, in Creation units (about 70 per
/// metre, so 8 is roughly 11 cm).
///
/// The camera has Bevy's default 0.1 near plane, so a doorway further than that from the camera
/// still projects to something meaningful and the polygon clipping handles it. The
/// margin is for the camera standing in or touching the doorway, where the rectangle is edge-on or
/// straddles the lens and a projected rectangle is numerically unreliable. It is also a few units
/// of slack for marker placement. Too large a value only draws more (the walk stays conservative);
/// 8 covers the player's body leaning into a doorway without opening doorways the player is
/// clearly beside. The image comparison against a real interior is where to retune it.
const PORTAL_OPEN_DISTANCE: f32 = 8.0;

/// Smallest clip-space `w` kept when clipping a doorway against the camera; anything nearer is
/// behind the lens.
const CLIP_W_EPSILON: f32 = 1.0e-3;

/// The owner cache is emptied past this many entries, which drops the despawned meshes in it.
const OWNER_CACHE_LIMIT: usize = 100_000;

/// One `room_bounds` row in cell-local runtime space.
#[derive(Debug, Clone, PartialEq)]
pub struct Room {
    /// `room_bounds.ref_id`.
    pub ref_id: u32,
    pub center: Vec3,
    pub rotation: Quat,
    /// Half-extents along the room's own runtime axes.
    pub half: Vec3,
}

impl Room {
    /// A room from table values: Creation position, Euler angles and half-extents.
    pub fn from_creation(
        ref_id: u32,
        position: [f32; 3],
        rotation: [f32; 3],
        half: [f32; 3],
    ) -> Self {
        Self {
            ref_id,
            center: Vec3::from_array(shared::coordinates::creation_to_runtime_vector(position)),
            rotation: Quat::from_array(shared::coordinates::creation_euler_to_runtime_quaternion(
                rotation,
            )),
            // Creation (x, y, z) are runtime (x, z, -y): a half-extent keeps its size.
            half: Vec3::new(half[0], half[2], half[1]),
        }
    }

    /// Whether a cell-local point is inside the box, edges included.
    pub fn contains(&self, point: Vec3) -> bool {
        let local = self.rotation.inverse() * (point - self.center);
        local.abs().cmple(self.half).all()
    }
}

/// A portal side distinguishes authored outside space from unresolved conversion data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSide {
    Room(usize),
    Unbounded,
    Unknown,
}

impl PortalSide {
    fn index(self, rooms: usize) -> Option<usize> {
        match self {
            Self::Room(index) if index < rooms => Some(index),
            Self::Unbounded => Some(rooms),
            _ => None,
        }
    }
}

/// Per-view volumes for the cell's rooms and its explicit outside space.
#[derive(Default)]
struct CellVisibility {
    rooms: Vec<RoomVisibility>,
    unbounded: RoomVisibility,
}

impl CellVisibility {
    fn get(&self, side: PortalSide) -> Option<&RoomVisibility> {
        match side {
            PortalSide::Room(index) => self.rooms.get(index),
            PortalSide::Unbounded => Some(&self.unbounded),
            PortalSide::Unknown => None,
        }
    }
}

/// One `room_portals` row in cell-local runtime space.
#[derive(Debug, Clone, PartialEq)]
pub struct Portal {
    /// `room_portals.ref_id`.
    pub ref_id: u32,
    /// The doorway's corners in order around the rectangle.
    pub corners: [Vec3; 4],
    /// Authored spaces on each side, with unresolved data kept distinct from outside.
    pub rooms: [PortalSide; 2],
}

impl Portal {
    /// A portal from table values. `rooms` are `room_bounds.ref_id`s looked up in `room_index`.
    pub fn from_creation(
        ref_id: u32,
        position: [f32; 3],
        rotation: [f32; 3],
        half: [f32; 2],
        rooms: [PortalSide; 2],
    ) -> Self {
        let center = Vec3::from_array(shared::coordinates::creation_to_runtime_vector(position));
        let rotation = Quat::from_array(shared::coordinates::creation_euler_to_runtime_quaternion(
            rotation,
        ));
        // The doorway is the marker's local X-Z rectangle in Creation axes: runtime local X and Y.
        let x = rotation * Vec3::X * half[0];
        let y = rotation * Vec3::Y * half[1];
        Self {
            ref_id,
            corners: [
                center - x - y,
                center + x - y,
                center + x + y,
                center - x + y,
            ],
            rooms,
        }
    }

    fn center(&self) -> Vec3 {
        self.corners.iter().copied().sum::<Vec3>() / 4.0
    }

    /// Distance from a point to the doorway rectangle.
    fn distance_to(&self, point: Vec3) -> f32 {
        let center = self.center();
        let along_x = self.corners[1] - self.corners[0];
        let along_y = self.corners[3] - self.corners[0];
        let (half_x, half_y) = (along_x.length() * 0.5, along_y.length() * 0.5);
        let (axis_x, axis_y) = (along_x.normalize_or_zero(), along_y.normalize_or_zero());
        let normal = axis_x.cross(axis_y).normalize_or_zero();
        let offset = point - center;
        let dx = (offset.dot(axis_x).abs() - half_x).max(0.0);
        let dy = (offset.dot(axis_y).abs() - half_y).max(0.0);
        let dn = offset.dot(normal).abs();
        Vec3::new(dx, dy, dn).length()
    }
}

/// Two room boxes closer than this many Creation units along a separating axis (touching or
/// overlapping included) count as one space. Stacked rooms in shipped cells leave seams of a few
/// units (Ansilvund01 has one of 7.6 between a floor and the ceiling below), which the level
/// designer meant as touching, so the tolerance is a little wider than that.
pub const OVERLAP_TOLERANCE: f32 = 8.0;

/// Whether two room boxes overlap or lie within [`OVERLAP_TOLERANCE`] of each other: the
/// separating-axis test on two oriented boxes (their six face axes and the nine edge cross
/// products).
pub fn rooms_overlap(a: &Room, b: &Room) -> bool {
    // A box with a non-finite number has no geometry to test. Joining it keeps the neighbours
    // drawn; "not overlapping" would drop an edge and cull more.
    if !(a.center.is_finite()
        && b.center.is_finite()
        && a.half.is_finite()
        && b.half.is_finite()
        && a.rotation.is_finite()
        && b.rotation.is_finite())
    {
        return true;
    }
    // The tolerance is in units only if the rotations are unit quaternions, which
    // `Room::from_creation` guarantees.
    debug_assert!(a.rotation.is_normalized() && b.rotation.is_normalized());
    let axes_a = [
        a.rotation * Vec3::X,
        a.rotation * Vec3::Y,
        a.rotation * Vec3::Z,
    ];
    let axes_b = [
        b.rotation * Vec3::X,
        b.rotation * Vec3::Y,
        b.rotation * Vec3::Z,
    ];
    let offset = b.center - a.center;
    let separated = |axis: Vec3| {
        let radius = |half: Vec3, axes: &[Vec3; 3]| {
            (0..3)
                .map(|i| half[i] * axes[i].dot(axis).abs())
                .sum::<f32>()
        };
        offset.dot(axis).abs()
            > radius(a.half, &axes_a) + radius(b.half, &axes_b) + OVERLAP_TOLERANCE
    };
    for axis in axes_a.into_iter().chain(axes_b) {
        if separated(axis) {
            return false;
        }
    }
    for edge_a in axes_a {
        for edge_b in axes_b {
            let axis = edge_a.cross(edge_b);
            // Parallel edges give no new axis.
            if axis.length_squared() > 1.0e-6 && separated(axis.normalize()) {
                return false;
            }
        }
    }
    true
}

/// The implicit open edges of a set of rooms: every pair of room indices whose boxes overlap.
/// O(rooms squared), done once when a cell's graph is built.
pub fn overlap_links(rooms: &[Room]) -> Vec<(usize, usize)> {
    let mut links = Vec::new();
    for (i, a) in rooms.iter().enumerate() {
        for (j, b) in rooms.iter().enumerate().skip(i + 1) {
            if rooms_overlap(a, b) {
                links.push((i, j));
            }
        }
    }
    links
}

/// Which rooms an object belongs to, as indices into [`RoomGraph::rooms`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Membership {
    /// Rooms whose box holds the object's pivot (`contained = 1`).
    pub contained: Vec<usize>,
    /// Rooms whose box only overlaps the object's bounds (`contained = 0`).
    pub overlapping: Vec<usize>,
}

/// The room graph of one interior cell, in the space of the cell root.
#[derive(Debug, Clone, Default)]
pub struct RoomGraph {
    pub(crate) authored: Option<assignment::Assignments>,
    /// Full export with authored endpoint provenance and complete reference identities.
    pub supports_unbounded: bool,
    /// Explicit per-cell metadata says some authored geometry could not be exported.
    pub incomplete: bool,
    pub rooms: Vec<Room>,
    pub portals: Vec<Portal>,
    /// `room_links` as undirected edges between room indices.
    pub links: Vec<(usize, usize)>,
    /// Sources of authored links whose targets could not be loaded in this cell.
    pub unresolved_link_rooms: HashSet<usize>,
    /// Implicit edges between rooms whose boxes overlap or touch ([`overlap_links`]). Unlike a link,
    /// an overlap is walked thick - narrowed to the neighbour's screen rectangle - between two rooms
    /// that both have a portal of the cell; reaching a neighbour without one draws the whole cell.
    pub overlaps: Vec<(usize, usize)>,
    /// Membership by placed-reference id. The key is one number under three names: the `FormId`
    /// component of a spawned reference (`streaming::spawn_cell` inserts `FormId(reference.form_id)`),
    /// `ReferenceRow::form_id`, which the world database loader fills from `references.id`
    /// (`world::database::REFERENCE_COLUMNS` starts with `r.id`), and `reference_rooms.reference_id`.
    /// It is the placement's id, not the base object's `base_form_id`. A test proves the chain
    /// through the real loader (`the_membership_key_is_the_reference_id_the_loader_delivers`).
    pub membership: HashMap<u32, Membership>,
}

/// An axis-aligned rectangle in normalised device coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    min: Vec2,
    max: Vec2,
}

/// The screen rectangle a room's box covers, with its edges clipped against the camera like a
/// doorway's: `None` when the whole box is behind the lens.
fn project_box(room: &Room, clip_from_local: Mat4) -> Option<Rect> {
    let corner = |index: usize| {
        let sign = |bit: usize| if index & bit == 0 { -1.0 } else { 1.0 };
        let local = Vec3::new(
            sign(1) * room.half.x,
            sign(2) * room.half.y,
            sign(4) * room.half.z,
        );
        clip_from_local * (room.center + room.rotation * local).extend(1.0)
    };
    let corners: Vec<Vec4> = (0..8).map(corner).collect();
    let mut points: Vec<Vec4> = Vec::with_capacity(20);
    for (index, &current) in corners.iter().enumerate() {
        if current.w >= CLIP_W_EPSILON {
            points.push(current);
        }
        // Each of the 12 edges once: from a corner to the one with a higher bit set.
        for bit in [1, 2, 4] {
            if index & bit != 0 {
                continue;
            }
            let next = corners[index | bit];
            if (current.w >= CLIP_W_EPSILON) != (next.w >= CLIP_W_EPSILON) {
                let t = (CLIP_W_EPSILON - current.w) / (next.w - current.w);
                points.push(current + (next - current) * t);
            }
        }
    }
    if points.is_empty() {
        return None;
    }
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for point in points {
        let ndc = Vec2::new(point.x, point.y) / point.w;
        min = min.min(ndc);
        max = max.max(ndc);
    }
    (min.is_finite() && max.is_finite()).then_some(Rect { min, max })
}

impl RoomGraph {
    /// Indices of the rooms whose box holds a cell-local point, in table order.
    pub fn rooms_containing(&self, point: Vec3) -> Vec<usize> {
        if let Some(authored) = &self.authored {
            return (0..self.rooms.len())
                .filter(|&index| {
                    authored.masters[index] == Some(index)
                        && authored.contains(&self.rooms, index, point)
                })
                .collect();
        }
        self.rooms
            .iter()
            .enumerate()
            .filter(|(_, room)| room.contains(point))
            .map(|(index, _)| index)
            .collect()
    }

    /// The rooms reachable from the rooms around `camera`, through doorways that lie inside the
    /// view, including the unbounded node. `None` means incomplete data requires full rendering.
    /// `clip_from_local` maps cell-local space to clip space.
    ///
    /// Every room that contains the camera is a start, which is the deterministic superset of
    /// "pick one": a camera in a doorway is in two boxes and sees into both.
    fn visibility_volumes(&self, camera: Vec3, clip_from_local: Mat4) -> Option<CellVisibility> {
        let full = VisibilityVolume::camera(clip_from_local)?;
        let room_count = self.rooms.len();
        if room_count == 0 || self.incomplete {
            return None;
        }
        if self.authored.as_ref().is_some_and(|authored| {
            !authored.complete || authored.masters.iter().any(Option::is_none)
        }) {
            return None;
        }
        let outside = room_count;
        let starts = self.rooms_containing(camera);
        if starts.is_empty() {
            // Room boxes do not prove outside-space coverage: the camera may be in a seam.
            return None;
        }
        let mut doorways: Vec<Vec<usize>> = vec![Vec::new(); room_count + 1];
        for (index, portal) in self.portals.iter().enumerate() {
            let [Some(a), Some(b)] = portal.rooms.map(|side| side.index(room_count)) else {
                // Unknown endpoints may conceal outside connectivity. Never interpret them as
                // authored outside, including when the camera starts outside all boxes.
                return None;
            };
            if a == b {
                continue;
            }
            doorways[a].push(index);
            doorways[b].push(index);
        }
        let mut linked: Vec<Vec<usize>> = vec![Vec::new(); room_count + 1];
        for &(a, b) in &self.links {
            if a != b && a < room_count && b < room_count {
                linked[a].push(b);
                linked[b].push(a);
            }
        }
        let mut overlapping: Vec<Vec<usize>> = vec![Vec::new(); room_count + 1];
        for &(a, b) in &self.overlaps {
            if a != b && a < room_count && b < room_count {
                overlapping[a].push(b);
                overlapping[b].push(a);
            }
        }
        // Rooms with no usable aperture still retain the open-hall fallback. The explicit
        // unbounded node can legitimately have no portals and does not trigger that fallback.
        let has_portal: Vec<bool> = doorways.iter().map(|portals| !portals.is_empty()).collect();
        let mut visibility: Vec<RoomVisibility> = (0..room_count + 1)
            .map(|_| RoomVisibility::default())
            .collect();
        let mut pending: Vec<_> = starts
            .into_iter()
            .map(|room| (room, full.clone()))
            .collect();
        while let Some((room, arriving)) = pending.pop() {
            if self.unresolved_link_rooms.contains(&room) {
                return None;
            }
            // Open halls can expose rooms across uncovered box gaps. Keep their conservative
            // fallback until authored joins and room assignment are available.
            if self.authored.is_none() && room != outside && !has_portal[room] {
                return None;
            }
            let Some(seen) = visibility[room].insert(arriving, &full) else {
                continue;
            };
            for &portal_index in &doorways[room] {
                let portal = &self.portals[portal_index];
                let [Some(a), Some(b)] = portal.rooms.map(|side| side.index(room_count)) else {
                    continue;
                };
                let other = if a == room { b } else { a };
                let through = if portal.distance_to(camera) <= PORTAL_OPEN_DISTANCE {
                    Some(seen.clone())
                } else {
                    seen.through_portal(&portal.corners, camera, clip_from_local)
                };
                if let Some(through) = through {
                    pending.push((other, through));
                }
            }
            for &other in &linked[room] {
                pending.push((other, seen.clone()));
            }
            // Clip implicit overlaps to the neighbour's box only when both rooms have portal
            // data. Queue an open neighbour without clipping so the whole-cell fallback runs.
            for &other in &overlapping[room] {
                if !has_portal[room] || !has_portal[other] {
                    pending.push((other, seen.clone()));
                    continue;
                }
                let destination = &self.rooms[other];
                if destination.contains(camera) {
                    pending.push((other, seen.clone()));
                    continue;
                }
                let bounds = MeshBounds::transformed(
                    &Aabb::from_min_max(-destination.half, destination.half),
                    Affine3A::from_rotation_translation(destination.rotation, destination.center),
                );
                if bounds
                    .as_ref()
                    .is_some_and(|bounds| !seen.intersects(bounds))
                {
                    continue;
                }
                if let Some(screen) = project_box(destination, clip_from_local) {
                    pending.push((
                        other,
                        seen.through_screen_rect(screen.min, screen.max, clip_from_local),
                    ));
                }
            }
        }
        let unbounded = visibility
            .pop()
            .expect("outside node allocated with the rooms");
        Some(CellVisibility {
            rooms: visibility,
            unbounded,
        })
    }

    /// Whether any membership volume intersects this mesh, or its policy exempts it.
    fn mesh_is_drawn(
        &self,
        form_id: u32,
        visible: &CellVisibility,
        bounds: Option<&MeshBounds>,
        reference_bounds: Option<&MeshBounds>,
    ) -> bool {
        let Some(membership) = self.membership.get(&form_id) else {
            return true;
        };
        if membership.contained.len() > 1 {
            return true;
        }
        // Portal overlap adds endpoint candidates. Keep geometric overlap candidates too:
        // conservative aggregate bounds can touch a portal without establishing exact ownership.
        if let Some(owner_bounds) = reference_bounds.or(bounds) {
            for portal in &self.portals {
                let attached = portal.rooms.iter().any(|side| match side {
                    PortalSide::Room(room) => membership.contained.contains(room),
                    PortalSide::Unbounded => {
                        self.supports_unbounded && membership.contained.is_empty()
                    }
                    PortalSide::Unknown => false,
                });
                if !attached || !owner_bounds.intersects_portal(&portal.corners) {
                    continue;
                }
                if portal.rooms.iter().any(|&side| {
                    visible
                        .get(side)
                        .is_none_or(|volume| volume.intersects(bounds))
                }) {
                    return true;
                }
            }
        }
        if membership.contained.is_empty() {
            if !self.supports_unbounded {
                return true;
            }
            if visible.unbounded.intersects(bounds) {
                return true;
            }
        }
        membership
            .contained
            .iter()
            .chain(&membership.overlapping)
            .any(|&room| {
                visible
                    .rooms
                    .get(room)
                    .is_none_or(|room| room.intersects(bounds))
            })
    }
}

/// The four tables of section 15 that the reader needs. `room_graph_stats` additionally opts into
/// verified outside endpoints; legacy graphs do not require it.
const REQUIRED_TABLES: [&str; 4] = [
    "room_bounds",
    "room_portals",
    "room_links",
    "reference_rooms",
];

/// Whether the database carries the room tables. A database converted before them still loads and
/// the feature is simply off.
pub fn room_tables_present(connection: &Connection) -> rusqlite::Result<bool> {
    let mut statement = connection
        .prepare_cached("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1")?;
    for table in REQUIRED_TABLES {
        if statement.query_row([table], |row| row.get::<_, i64>(0))? == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Reads the room graph of one interior cell. The tables must be present
/// ([`room_tables_present`]); a cell without rooms yields an empty graph.
pub fn load_room_graph(connection: &Connection, cell_id: u32) -> rusqlite::Result<RoomGraph> {
    let mut graph = RoomGraph::default();
    let mut room_index: HashMap<u32, usize> = HashMap::new();
    {
        let mut statement = connection.prepare_cached(
            "SELECT ref_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z \
             FROM room_bounds WHERE cell_id=?1 ORDER BY ref_id",
        )?;
        let rows = statement.query_map([cell_id], |row| {
            Ok(Room::from_creation(
                row.get(0)?,
                [row.get(1)?, row.get(2)?, row.get(3)?],
                [row.get(4)?, row.get(5)?, row.get(6)?],
                [row.get(7)?, row.get(8)?, row.get(9)?],
            ))
        })?;
        for room in rows {
            let room = room?;
            room_index.insert(room.ref_id, graph.rooms.len());
            graph.rooms.push(room);
        }
    }
    if graph.rooms.is_empty() {
        return Ok(graph);
    }
    let completeness_present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='room_graph_cells')",
        [],
        |row| row.get(0),
    )?;
    let complete: Option<bool> = if completeness_present {
        connection.query_row(
            "SELECT (SELECT complete FROM room_graph_cells WHERE cell_id=?1)",
            [cell_id],
            |row| row.get(0),
        )?
    } else {
        None
    };
    graph.incomplete = complete == Some(false)
        || graph.rooms.iter().any(|room| {
            !room.center.is_finite()
                || !room.rotation.is_finite()
                || !room.half.is_finite()
                || !room.half.cmpge(Vec3::ZERO).all()
        });
    let endpoint_columns: bool = connection.query_row(
        "SELECT count(*)=2 FROM pragma_table_info('room_portals') WHERE name IN ('unbounded_a','unbounded_b')", [], |row| row.get(0))?;
    let stats_present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='room_graph_stats')",
        [],
        |row| row.get(0),
    )?;
    graph.supports_unbounded = complete == Some(true) && endpoint_columns && stats_present && connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='visibility_version' AND value=2)", [], |row| row.get::<_,bool>(0))?;
    {
        let provenance = if graph.supports_unbounded {
            "unbounded_a,unbounded_b"
        } else {
            "0,0"
        };
        let mut statement = connection.prepare_cached(&format!(
            "SELECT ref_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_z,room_a,room_b,{provenance} FROM room_portals WHERE cell_id=?1 ORDER BY ref_id"))?;
        let rows = statement.query_map([cell_id], |row| {
            let side = |index: usize| -> rusqlite::Result<PortalSide> {
                let id = row.get::<_, Option<u32>>(index)?;
                Ok(match id {
                    Some(id) => room_index
                        .get(&id)
                        .copied()
                        .map(PortalSide::Room)
                        .unwrap_or(PortalSide::Unknown),
                    None if row.get::<_, bool>(index + 2)? => PortalSide::Unbounded,
                    None => PortalSide::Unknown,
                })
            };
            let half: [f32; 2] = [row.get(7)?, row.get(8)?];
            Ok((
                Portal::from_creation(
                    row.get(0)?,
                    [row.get(1)?, row.get(2)?, row.get(3)?],
                    [row.get(4)?, row.get(5)?, row.get(6)?],
                    half,
                    [side(9)?, side(10)?],
                ),
                half.iter().all(|value| value.is_finite() && *value >= 0.0),
            ))
        })?;
        for portal in rows {
            let (portal, valid) = portal?;
            graph.incomplete |= !valid || portal.corners.iter().any(|point| !point.is_finite());
            graph.portals.push(portal);
        }
    }
    {
        let mut statement = connection.prepare_cached(
            "SELECT l.room_ref,l.linked_ref FROM room_links l \
             JOIN room_bounds b ON b.ref_id=l.room_ref WHERE b.cell_id=?1 \
             ORDER BY l.room_ref,l.linked_ref",
        )?;
        let rows = statement.query_map([cell_id], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?))
        })?;
        for link in rows {
            let (room, linked) = link?;
            // The target is stored as written, even when it is not a room of this cell.
            if let Some(&a) = room_index.get(&room) {
                if let Some(&b) = room_index.get(&linked) {
                    graph.links.push((a, b));
                } else {
                    graph.unresolved_link_rooms.insert(a);
                }
            }
        }
    }
    if graph.supports_unbounded {
        graph.authored = assignment::load(connection, cell_id, &graph.rooms, &mut graph.portals)?;
    }
    if graph.authored.is_some() {
        graph.links.clear();
        graph.unresolved_link_rooms.clear();
    }
    let implicit: Vec<(usize, usize)> = if graph.authored.is_some() {
        Vec::new()
    } else {
        overlap_links(&graph.rooms)
        .into_iter()
        .filter(|&(a, b)| {
            let joined = |&(x, y): &(usize, usize)| (x, y) == (a, b) || (x, y) == (b, a);
            !graph.links.iter().any(joined)
                && !graph
                    .portals
                    .iter()
                    .any(|portal| matches!(portal.rooms, [PortalSide::Room(x), PortalSide::Room(y)] if joined(&(x, y))))
        })
        .collect()
    };
    if !implicit.is_empty() {
        info!(
            "room culling: cell {cell_id:08X} has {} implicit overlap edges between touching or overlapping rooms",
            implicit.len()
        );
    }
    graph.overlaps = implicit;
    {
        let mut statement = connection.prepare_cached(
            "SELECT m.reference_id,m.room_ref,m.contained FROM reference_rooms m \
             JOIN room_bounds b ON b.ref_id=m.room_ref WHERE b.cell_id=?1 \
             ORDER BY m.reference_id,m.room_ref",
        )?;
        let rows = statement.query_map([cell_id], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })?;
        for row in rows {
            let (reference, room, contained) = row?;
            let Some(&room) = room_index.get(&room) else {
                continue;
            };
            let membership = graph.membership.entry(reference).or_default();
            if contained {
                membership.contained.push(room);
            } else {
                membership.overlapping.push(room);
            }
        }
    }
    if graph.supports_unbounded {
        let references_present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='references')",
            [],
            |row| row.get(0),
        )?;
        if references_present {
            let mut statement =
                connection.prepare_cached("SELECT id FROM \"references\" WHERE cell_id=?1")?;
            for id in statement.query_map([cell_id], |row| row.get::<_, u32>(0))? {
                graph.membership.entry(id?).or_default();
            }
        }
    }
    Ok(graph)
}

/// Whether the database has the room tables, once known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableProbe {
    Unknown,
    Present,
    Absent,
}

/// A loaded interior cell root: a streamed cell without an exterior grid position.
type InteriorRoot = (With<StreamedCellRoot>, Without<ExteriorCellGrid>);

/// What a background read of one cell's room graph found.
type LoadResult = Result<Option<RoomGraph>, String>;

struct LoadedGraph {
    root: Entity,
    graph: RoomGraph,
}

/// The streaming camera's `Mesh3d` list in one frame: what Bevy's frustum check listed and what
/// room culling left drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct MainViewMeshes {
    pub listed: usize,
    pub drawn: usize,
}

/// The probe result and the graphs of the interior cells that are loaded.
#[derive(Resource)]
pub struct RoomCullingState {
    database_path: PathBuf,
    probe: TableProbe,
    graphs: HashMap<u32, LoadedGraph>,
    pending: HashMap<u32, Task<LoadResult>>,
    /// Cells whose read failed; they are not retried.
    failed: HashSet<u32>,
    /// Entities removed from the room-culling views in the last frame, for diagnostics.
    pub culled_last_frame: usize,
    /// The streaming camera's `Mesh3d` list in the last frame, before and after room culling.
    /// `None` before a main view is checked. Capture receipts record it, so a shots run measures the
    /// reduction at every pose.
    pub main_view_meshes: Option<MainViewMeshes>,
    /// Whether the first frame that hid anything has been logged.
    reported_first_cull: bool,
}

impl RoomCullingState {
    fn new(database_path: PathBuf) -> Self {
        Self {
            database_path,
            probe: TableProbe::Unknown,
            graphs: HashMap::new(),
            pending: HashMap::new(),
            failed: HashSet::new(),
            culled_last_frame: 0,
            main_view_meshes: None,
            reported_first_cull: false,
        }
    }

    /// Whether the database has the tables (or has not been probed yet).
    pub fn active(&self) -> bool {
        self.probe != TableProbe::Absent
    }

    /// Room graphs of loaded interior cells still being read. A shot waits for them: until a
    /// cell's graph arrives, its rooms are drawn whole and the view is not the culled one.
    pub fn pending_reads(&self) -> usize {
        self.pending.len()
    }

    /// Loaded interior cells whose room graph could not be read; they are drawn whole.
    pub fn failed_reads(&self) -> usize {
        self.failed.len()
    }

    /// Registers a graph for a loaded cell root. Tests only: the loader goes through
    /// `collect_loaded_graphs`, which also records what the probe found.
    #[cfg(test)]
    pub fn insert_graph(&mut self, cell_id: u32, root: Entity, graph: RoomGraph) {
        self.graphs.insert(cell_id, LoadedGraph { root, graph });
    }
}

/// Updates graph-read state before live capture readiness is observed.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RoomGraphReadiness;

/// Registers automatic interior room culling. Missing room tables leave every mesh drawn.
pub struct RoomCullingPlugin {
    pub database_path: PathBuf,
}

impl RoomCullingPlugin {
    pub fn from_config(config: &EngineConfig) -> Self {
        Self {
            database_path: config.assets_dir.join("skyrim_world.db"),
        }
    }
}

impl Plugin for RoomCullingPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(RoomCullingState::new(self.database_path.clone()));
        app.add_systems(
            Update,
            (
                collect_loaded_graphs,
                drop_stale_graphs,
                request_room_graphs,
            )
                .chain()
                .in_set(RoomGraphReadiness),
        )
        .add_systems(
            PostUpdate,
            cull_room_views
                .in_set(VisibilitySystems::CheckVisibility)
                // The list is only complete once Bevy's own check has filled it.
                .after(check_visibility_cpu_culling),
        );
    }
}

/// Starts a background read for every loaded interior cell that has no graph yet.
fn request_room_graphs(
    mut state: ResMut<RoomCullingState>,
    roots: Query<(Entity, &CellRef), InteriorRoot>,
) {
    if !state.active() {
        return;
    }
    for (_, cell) in &roots {
        let cell_id = cell.0;
        if state.graphs.contains_key(&cell_id)
            || state.pending.contains_key(&cell_id)
            || state.failed.contains(&cell_id)
        {
            continue;
        }
        let path = state.database_path.clone();
        let task = AsyncComputeTaskPool::get().spawn(async move { read_graph(&path, cell_id) });
        state.pending.insert(cell_id, task);
    }
}

fn read_graph(path: &std::path::Path, cell_id: u32) -> LoadResult {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    if !room_tables_present(&connection).map_err(|error| error.to_string())? {
        return Ok(None);
    }
    load_room_graph(&connection, cell_id)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Takes the finished background reads.
fn collect_loaded_graphs(
    mut state: ResMut<RoomCullingState>,
    roots: Query<(Entity, &CellRef), InteriorRoot>,
) {
    let finished: Vec<(u32, LoadResult)> = state
        .pending
        .iter_mut()
        .filter_map(|(&cell, task)| {
            future::block_on(future::poll_once(task)).map(|result| (cell, result))
        })
        .collect();
    for (cell_id, result) in finished {
        state.pending.remove(&cell_id);
        match result {
            Ok(None) => {
                if state.probe != TableProbe::Absent {
                    info!(
                        "room culling: {} has no room tables (converted before the room graph); \
                         the feature stays off and every object is drawn",
                        state.database_path.display()
                    );
                }
                state.probe = TableProbe::Absent;
            }
            Ok(Some(graph)) => {
                if state.probe == TableProbe::Unknown {
                    info!("room culling: room tables found");
                }
                state.probe = TableProbe::Present;
                let Some(root) = roots
                    .iter()
                    .find_map(|(entity, cell)| (cell.0 == cell_id).then_some(entity))
                else {
                    continue;
                };
                info!(
                    "room culling: cell {cell_id:08X} has {} rooms, {} portals, {} links, {} placed objects",
                    graph.rooms.len(),
                    graph.portals.len(),
                    graph.links.len(),
                    graph.membership.len()
                );
                state.graphs.insert(cell_id, LoadedGraph { root, graph });
            }
            Err(error) => {
                warn!("room culling: cell {cell_id:08X} room graph not read: {error}");
                state.failed.insert(cell_id);
            }
        }
    }
}

/// Forgets graphs, failures and reads of cells whose root was unloaded; a cell that comes back
/// is read again.
fn drop_stale_graphs(
    mut state: ResMut<RoomCullingState>,
    roots: Query<(Entity, &CellRef), With<StreamedCellRoot>>,
) {
    if state.graphs.is_empty() && state.failed.is_empty() && state.pending.is_empty() {
        return;
    }
    let live: HashSet<u32> = roots.iter().map(|(_, cell)| cell.0).collect();
    state
        .graphs
        .retain(|_, loaded| roots.get(loaded.root).is_ok());
    state.failed.retain(|cell| live.contains(cell));
    // Dropping a task cancels it.
    state.pending.retain(|cell, _| live.contains(cell));
}

/// Filters each room-culling view's `Mesh3d` list against its room visibility volumes.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn cull_room_views(
    mut state: ResMut<RoomCullingState>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
    roots: Query<&GlobalTransform, With<StreamedCellRoot>>,
    mut views: Query<
        (
            &GlobalTransform,
            &Projection,
            Option<&Camera>,
            Has<StreamingCamera>,
            &mut VisibleEntities,
        ),
        Or<(With<StreamingCamera>, With<RoomCullingView>)>,
    >,
    mesh_bounds: Query<(&Aabb, &GlobalTransform), Without<NoFrustumCulling>>,
    reference_bounds: Query<(&ExpectedModelBounds, &GlobalTransform)>,
    reference_transforms: Query<&GlobalTransform>,
    parents: Query<&ChildOf>,
    ids: Query<&FormId>,
    exemptions: Query<(
        Option<&RigidBody>,
        Has<RoomCullExempt>,
        Has<RoomCullLegacyExempt>,
        Has<ExpectedModelBounds>,
    )>,
    mut owners: Local<HashMap<Entity, (u32, Entity)>>,
    mut assignments: Local<RoomAssignmentCache>,
) {
    state.culled_last_frame = 0;
    state.main_view_meshes = None;
    let mut cells: Vec<u32> = state.graphs.keys().copied().collect();
    cells.sort_unstable();
    let mut culled = 0;
    let mut main_view = None;
    assignments.0.retain(|&(cell, root, reference), _| {
        state
            .graphs
            .get(&cell)
            .is_some_and(|loaded| loaded.root == root)
            && reference_transforms.get(reference).is_ok()
    });
    for (camera_transform, projection, camera, is_main, mut visible) in &mut views {
        if camera.is_some_and(|camera| !camera.is_active) {
            continue;
        }
        let listed = visible.get(TypeId::of::<Mesh3d>()).len();
        if settings
            .as_ref()
            .is_some_and(|settings| !settings.room_culling)
        {
            if is_main {
                main_view = Some(MainViewMeshes {
                    listed,
                    drawn: listed,
                });
            }
            continue;
        }
        let clip_from_world =
            projection.get_clip_from_view() * camera_transform.to_matrix().inverse();
        for cell in &cells {
            let loaded = &state.graphs[cell];
            let Ok(root) = roots.get(loaded.root) else {
                continue;
            };
            let world_from_local = root.to_matrix();
            let local_from_world = root.affine().inverse();
            let camera_local = local_from_world.transform_point3(camera_transform.translation());
            let Some(volumes) = loaded
                .graph
                .visibility_volumes(camera_local, clip_from_world * world_from_local)
            else {
                continue;
            };
            let list = visible.get_mut(TypeId::of::<Mesh3d>());
            let before = list.len();
            // The owner of a mesh does not change while it lives, so a found owner is remembered
            // (entity ids carry a generation, so a recycled slot is a different key). A mesh with
            // no owner is not cached: its hierarchy may still be being built.
            if owners.len() > OWNER_CACHE_LIMIT {
                owners.clear();
            }
            list.retain(|&entity| {
                let owner = match owners.get(&entity) {
                    Some(&owner) => Some(owner),
                    None => {
                        let found = owner_form_id(entity, &parents, &ids);
                        if let Some(owner) = found {
                            owners.insert(entity, owner);
                        }
                        found
                    }
                };
                let Some((form_id, reference)) = owner else {
                    return true;
                };
                // Re-read every frame: a body can be added after the mesh spawns.
                let has_bound = loaded
                    .graph
                    .authored
                    .as_ref()
                    .is_some_and(|authored| authored.has_source_bound(form_id));
                let exempted = |entity| {
                    exemptions
                        .get(entity)
                        .is_ok_and(|(body, marked, legacy, has_model_bounds)| {
                            marked
                                // Authored rules and source spheres supply ownership independently.
                            || (entity == reference
                                && !has_model_bounds
                                && loaded.graph.authored.is_none())
                                || (legacy && loaded.graph.authored.is_none())
                                || (body.is_some_and(|body| body_moves(*body))
                                    && (entity != reference || !has_bound))
                        })
                };
                let mut ancestor = entity;
                for _ in 0..=32 {
                    if exempted(ancestor) {
                        return true;
                    }
                    if ancestor == reference {
                        break;
                    }
                    let Ok(parent) = parents.get(ancestor) else {
                        return true;
                    };
                    ancestor = parent.parent();
                }
                if ancestor != reference {
                    return true;
                }
                let bounds = mesh_bounds.get(entity).ok().and_then(|(aabb, transform)| {
                    MeshBounds::transformed(aabb, local_from_world * transform.affine())
                });
                if let Some(authored) = &loaded.graph.authored {
                    if !authored.has_rule(form_id) {
                        return true;
                    }
                    let Ok(matrix) = reference_transforms.get(reference) else {
                        return true;
                    };
                    let transform = local_from_world * matrix.affine();
                    let assigned = assignments
                        .0
                        .entry((*cell, loaded.root, reference))
                        .or_insert_with(|| assignment::CachedAssignment {
                            transform,
                            owners: authored.initial_owners(
                                form_id,
                                transform,
                                &loaded.graph.rooms,
                                &loaded.graph.portals,
                            ),
                        });
                    if !transform.abs_diff_eq(assigned.transform, 1.0e-3) {
                        assigned.transform = transform;
                        assigned.owners = authored.owners(
                            form_id,
                            Some(transform),
                            &loaded.graph.rooms,
                            &loaded.graph.portals,
                        );
                    }
                    return assignment::draws(
                        &assigned.owners,
                        &loaded.graph.portals,
                        &volumes,
                        bounds.as_ref(),
                    );
                }
                let owner_bounds =
                    reference_bounds
                        .get(reference)
                        .ok()
                        .and_then(|(bounds, transform)| {
                            MeshBounds::transformed(
                                &Aabb::from_min_max(bounds.min, bounds.max),
                                local_from_world * transform.affine(),
                            )
                        });
                loaded.graph.mesh_is_drawn(
                    form_id,
                    &volumes,
                    bounds.as_ref(),
                    owner_bounds.as_ref(),
                )
            });
            culled += before - list.len();
            // No `break`: every resident interior is kept until something unloads it, so a second
            // cell whose rooms also hold the camera is walked too. A cell only removes its own
            // objects ([`RoomGraph::mesh_is_drawn`] keeps what is outside its graph).
        }
        if is_main {
            let drawn = visible.get(TypeId::of::<Mesh3d>()).len();
            main_view = Some(MainViewMeshes { listed, drawn });
        }
    }
    state.culled_last_frame = culled;
    state.main_view_meshes = main_view;
    if culled > 0 && !state.reported_first_cull {
        state.reported_first_cull = true;
        info!("room culling: the first frame that hid anything removed {culled} meshes");
    }
}

/// Whether a rigid body can move, as opposed to the `Fixed` bodies static collision uses.
fn body_moves(body: RigidBody) -> bool {
    !matches!(body, RigidBody::Fixed)
}

/// The FormID of the placed reference a mesh entity belongs to, and that reference's entity: the
/// entity's own, or its nearest ancestor's.
fn owner_form_id(
    entity: Entity,
    parents: &Query<&ChildOf>,
    ids: &Query<&FormId>,
) -> Option<(u32, Entity)> {
    let mut current = entity;
    for _ in 0..32 {
        if let Ok(id) = ids.get(current) {
            return Some((id.0, current));
        }
        current = parents.get(current).ok()?.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::database::{CellKey, DatabaseRequest, WorldDatabase};
    use bevy::{
        asset::AssetPlugin,
        camera::{
            primitives::{Aabb, Frustum},
            visibility::{
                CubemapVisibleEntities, ViewVisibility, VisibilityPlugin, VisibleMeshEntities,
            },
        },
        ecs::system::RunSystemOnce,
        math::primitives::ViewFrustum,
        transform::TransformPlugin,
    };

    fn reached_rooms(graph: &RoomGraph, camera: Vec3, clip: Mat4) -> Option<Vec<bool>> {
        graph.visibility_volumes(camera, clip).map(|visible| {
            visible
                .rooms
                .iter()
                .map(RoomVisibility::is_reached)
                .collect()
        })
    }

    fn visibility_for_rooms(reached: &[bool]) -> CellVisibility {
        let full = VisibilityVolume::camera(clip_from_local()).unwrap();
        let rooms = reached
            .iter()
            .map(|&reached| {
                let mut room = RoomVisibility::default();
                if reached {
                    room.insert(full.clone(), &full);
                }
                room
            })
            .collect();
        CellVisibility {
            rooms,
            unbounded: RoomVisibility::default(),
        }
    }

    /// The section 15 tables, as the converter writes them.
    const SCHEMA: &str = "
        CREATE TABLE room_bounds (
            ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
            pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
            rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
            half_x REAL NOT NULL, half_y REAL NOT NULL, half_z REAL NOT NULL,
            lighting_template_id INTEGER, image_space_id INTEGER,
            flags INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE room_portals (
            ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
            pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
            rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
            half_x REAL NOT NULL, half_z REAL NOT NULL,
            room_a INTEGER, room_b INTEGER);
        CREATE TABLE room_links (
            room_ref INTEGER NOT NULL, linked_ref INTEGER NOT NULL,
            PRIMARY KEY (room_ref, linked_ref));
        CREATE TABLE reference_rooms (
            reference_id INTEGER NOT NULL, room_ref INTEGER NOT NULL, contained INTEGER NOT NULL,
            PRIMARY KEY (reference_id, room_ref)) WITHOUT ROWID;
        CREATE TABLE room_graph_stats (name TEXT PRIMARY KEY, value INTEGER NOT NULL);";

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
    }

    fn authored_database() -> Connection {
        let connection = database();
        connection.execute_batch(
            "ALTER TABLE room_portals ADD COLUMN unbounded_a INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE room_portals ADD COLUMN unbounded_b INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE room_links ADD COLUMN link_index INTEGER NOT NULL DEFAULT 0;
             INSERT INTO room_graph_stats VALUES ('visibility_version',2),('assignment_version',1);
             CREATE TABLE room_reference_rules(reference_id INTEGER,cell_id INTEGER,assignment TEXT,target_ref INTEGER);
             CREATE TABLE room_model_bounds(base_form_id INTEGER,center_x REAL,center_y REAL,center_z REAL,radius REAL);
             CREATE TABLE room_graph_cells(cell_id INTEGER,complete INTEGER);
             INSERT INTO room_graph_cells VALUES(7,1);
             CREATE TABLE \"references\"(id INTEGER,cell_id INTEGER,base_form_id INTEGER,
                 pos_x REAL DEFAULT 0,pos_y REAL DEFAULT 0,pos_z REAL DEFAULT 0,
                 rot_x REAL DEFAULT 0,rot_y REAL DEFAULT 0,rot_z REAL DEFAULT 0,scale REAL DEFAULT 1);"
        ).unwrap();
        connection
    }

    #[test]
    fn authored_join_redirects_portals_and_preserves_disjoint_bounds() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [100.0, 0.0, 0.0]);
        insert_room(&connection, 3, 7, [200.0, 0.0, 0.0]);
        connection
            .execute_batch(
                "UPDATE room_bounds SET flags=256 WHERE ref_id IN (1,3);
             INSERT INTO room_links VALUES(1,2,0),(2,1,0);
             INSERT INTO room_portals VALUES(10,7,150,0,0,0,0,0,4,6,2,3,0,0);",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert!(graph.authored.is_some());
        assert_eq!(graph.rooms_containing(Vec3::new(100.0, 0.0, 0.0)), [0]);
        assert!(graph.rooms_containing(Vec3::new(50.0, 0.0, 0.0)).is_empty());
        assert_eq!(
            graph.portals[0].rooms,
            [PortalSide::Room(0), PortalSide::Room(2)]
        );
        assert!(graph.links.is_empty() && graph.overlaps.is_empty());
    }

    #[test]
    fn an_initial_named_target_does_not_require_legacy_stored_bounds() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [0.0, 25.0, 0.0]);
        connection
            .execute_batch(
                "INSERT INTO \"references\"(id,cell_id,base_form_id) VALUES(2,7,99);
             INSERT INTO room_reference_rules VALUES(2,7,'target',2);",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        let mut scene = scene();
        scene
            .app
            .world_mut()
            .resource_mut::<RoomCullingState>()
            .graphs
            .get_mut(&7)
            .unwrap()
            .graph = graph;
        let reference = scene
            .app
            .world()
            .get::<ChildOf>(scene.far)
            .unwrap()
            .parent();
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .remove::<ExpectedModelBounds>()
            .insert(GlobalTransform::IDENTITY);
        scene.app.world_mut().entity_mut(scene.far).insert((
            Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, -25.0)),
        ));
        run_cull(&mut scene.app);
        assert!(
            !mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "an explicit initial target supplies ownership without geometric memberships or spheres"
        );
    }

    #[test]
    fn current_source_bound_assignment_replaces_stale_membership_for_a_moving_reference() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [0.0, 25.0, 0.0]);
        connection
            .execute_batch(
                "INSERT INTO \"references\"(id,cell_id,base_form_id) VALUES(2,7,99);
             INSERT INTO room_reference_rules VALUES(2,7,'auto',NULL);
             INSERT INTO room_model_bounds VALUES(99,0,0,-25,1);
             INSERT INTO reference_rooms VALUES(2,1,1);",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        let mut scene = scene();
        scene
            .app
            .world_mut()
            .resource_mut::<RoomCullingState>()
            .graphs
            .get_mut(&7)
            .unwrap()
            .graph = graph;
        let reference = scene
            .app
            .world()
            .get::<ChildOf>(scene.far)
            .unwrap()
            .parent();
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .remove::<ExpectedModelBounds>()
            .insert((
                GlobalTransform::IDENTITY,
                RigidBody::Dynamic,
                RoomCullLegacyExempt,
            ));
        scene.app.world_mut().entity_mut(scene.far).insert((
            Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, -25.0)),
        ));
        run_cull(&mut scene.app);
        assert!(
            !mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "bound center is in the unreached room despite its pivot membership"
        );
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .insert(GlobalTransform::from_translation(Vec3::new(0.0, 0.0, 20.0)));
        scene
            .app
            .world_mut()
            .entity_mut(scene.far)
            .insert(GlobalTransform::from_translation(Vec3::new(0.0, 0.0, -5.0)));
        scene
            .app
            .world_mut()
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .push(scene.far);
        run_cull(&mut scene.app);
        assert!(
            mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "movement reassigns ownership before filtering this view"
        );
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .insert(GlobalTransform::IDENTITY);
        let moving_child = scene
            .app
            .world_mut()
            .spawn((ChildOf(reference), RigidBody::Dynamic))
            .id();
        scene.app.world_mut().entity_mut(scene.far).insert((
            ChildOf(moving_child),
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, -25.0)),
        ));
        run_cull(&mut scene.app);
        assert!(
            mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "an independently moving ancestor invalidates the reference's fixed source sphere"
        );
    }

    fn insert_room(connection: &Connection, id: u32, cell: u32, position: [f32; 3]) {
        connection
            .execute(
                "INSERT INTO room_bounds (ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z) \
                 VALUES (?1,?2,?3,?4,?5,0,0,0,10,20,5)",
                rusqlite::params![id, cell, position[0], position[1], position[2]],
            )
            .unwrap();
    }

    #[test]
    fn a_database_without_the_tables_has_the_feature_off() {
        let connection = Connection::open_in_memory().unwrap();
        assert!(!room_tables_present(&connection).unwrap());
        // One table is not enough.
        connection
            .execute_batch("CREATE TABLE room_bounds (ref_id INTEGER);")
            .unwrap();
        assert!(!room_tables_present(&connection).unwrap());
    }

    #[test]
    fn a_database_with_the_tables_is_detected() {
        assert!(room_tables_present(&database()).unwrap());
    }

    #[test]
    fn loader_preserves_outside_provenance_and_outside_reference_ids() {
        let connection = database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        connection
            .execute_batch(
                "ALTER TABLE room_portals ADD COLUMN unbounded_a INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE room_portals ADD COLUMN unbounded_b INTEGER NOT NULL DEFAULT 0;
             INSERT INTO room_graph_stats VALUES ('visibility_version',2);
             CREATE TABLE room_graph_cells (cell_id INTEGER PRIMARY KEY,complete INTEGER NOT NULL);
             INSERT INTO room_graph_cells VALUES (7,1);
             CREATE TABLE \"references\" (id INTEGER,cell_id INTEGER);
             INSERT INTO \"references\" VALUES (100,7),(101,8);
             INSERT INTO room_portals VALUES(10,7,0,0,0,0,0,0,4,6,1,NULL,0,1);",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert!(graph.supports_unbounded);
        assert_eq!(
            graph.portals[0].rooms,
            [PortalSide::Room(0), PortalSide::Unbounded]
        );
        assert_eq!(graph.membership[&100], Membership::default());
        assert!(!graph.membership.contains_key(&101));
        // An inconsistent foreign-room ID must not acquire outside meaning from a flag.
        connection
            .execute("UPDATE room_portals SET room_b=999", [])
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert_eq!(graph.portals[0].rooms[1], PortalSide::Unknown);
    }

    #[test]
    fn incomplete_exported_cells_draw_all_without_disabling_complete_neighbors() {
        let connection = database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 8, [0.0; 3]);
        connection
            .execute_batch(
                "ALTER TABLE room_portals ADD COLUMN unbounded_a INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE room_portals ADD COLUMN unbounded_b INTEGER NOT NULL DEFAULT 0;
             INSERT INTO room_graph_stats VALUES ('visibility_version',2);
             CREATE TABLE room_graph_cells (cell_id INTEGER PRIMARY KEY, complete INTEGER NOT NULL);
             INSERT INTO room_graph_cells VALUES (7,0),(8,1);
             CREATE TABLE [references] (id INTEGER,cell_id INTEGER);
             INSERT INTO room_portals VALUES(10,7,0,0,0,0,0,0,4,6,1,NULL,0,1),(11,8,0,0,0,0,0,0,4,6,2,NULL,0,1);",
            )
            .unwrap();
        let camera = Vec3::ZERO;
        let incomplete = load_room_graph(&connection, 7).unwrap();
        assert!(reached_rooms(&incomplete, camera, clip_from_local()).is_none());
        let complete = load_room_graph(&connection, 8).unwrap();
        assert!(reached_rooms(&complete, camera, clip_from_local()).is_some());
        connection
            .execute("DROP TABLE room_graph_cells", [])
            .unwrap();
        let legacy = load_room_graph(&connection, 8).unwrap();
        assert!(reached_rooms(&legacy, camera, clip_from_local()).is_none());
    }

    #[test]
    fn a_cell_without_rooms_loads_as_an_empty_graph() {
        let connection = database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        let graph = load_room_graph(&connection, 99).unwrap();
        assert!(graph.rooms.is_empty() && graph.portals.is_empty() && graph.membership.is_empty());
        // And an empty graph never hides anything.
        assert!(reached_rooms(&graph, Vec3::ZERO, Mat4::IDENTITY).is_none());
        assert!(graph.mesh_is_drawn(5, &CellVisibility::default(), None, None));
    }

    #[test]
    fn the_graph_is_read_in_runtime_axes() {
        let connection = database();
        insert_room(&connection, 1, 7, [100.0, 200.0, 50.0]);
        insert_room(&connection, 2, 7, [300.0, 200.0, 50.0]);
        insert_room(&connection, 3, 8, [0.0; 3]);
        connection
            .execute(
                "INSERT INTO room_portals VALUES (10,7,200,200,50,0,0,0,4,6,1,2)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO room_portals VALUES (11,7,0,0,0,0,0,0,4,6,1,NULL)",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO room_links VALUES (1,2),(1,9999),(3,1)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO reference_rooms VALUES (500,1,1),(500,2,0),(501,2,1),(502,3,1)",
                [],
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert_eq!(graph.rooms.len(), 2);
        // Creation (100, 200, 50) is runtime (100, 50, -200); half-extents (10, 20, 5) become
        // (10, 5, 20).
        assert_eq!(graph.rooms[0].center, Vec3::new(100.0, 50.0, -200.0));
        assert_eq!(graph.rooms[0].half, Vec3::new(10.0, 5.0, 20.0));
        assert!(graph.rooms[0].contains(Vec3::new(105.0, 52.0, -190.0)));
        assert!(!graph.rooms[0].contains(Vec3::new(105.0, 60.0, -190.0)));
        assert_eq!(graph.portals.len(), 2);
        // The doorway is the local X-Z rectangle in Creation axes: runtime X by runtime Y.
        let portal = &graph.portals[0];
        assert_eq!(portal.rooms, [PortalSide::Room(0), PortalSide::Room(1)]);
        assert!(portal.corners[0].distance(Vec3::new(196.0, 44.0, -200.0)) < 1.0e-3);
        assert!(portal.corners[2].distance(Vec3::new(204.0, 56.0, -200.0)) < 1.0e-3);
        // A missing side stays missing.
        assert_eq!(
            graph.portals[1].rooms,
            [PortalSide::Room(0), PortalSide::Unknown]
        );
        // Links outside the cell, or to a non-room, are dropped; the one from another cell too.
        assert_eq!(graph.links, vec![(0, 1)]);
        // The other cell's reference is not in this cell's membership.
        assert!(!graph.membership.contains_key(&502));
        assert_eq!(
            graph.membership[&500],
            Membership {
                contained: vec![0],
                overlapping: vec![1]
            }
        );
    }

    // --- Culling on synthetic graphs ---------------------------------------------------------

    fn room(ref_id: u32, center: Vec3, half: f32) -> Room {
        Room {
            ref_id,
            center,
            rotation: Quat::IDENTITY,
            half: Vec3::splat(half),
        }
    }

    /// A doorway in an XY plane at `center`, `half` wide and high, between two rooms.
    fn doorway(ref_id: u32, center: Vec3, half: f32, rooms: [usize; 2]) -> Portal {
        Portal {
            ref_id,
            corners: [
                center + Vec3::new(-half, -half, 0.0),
                center + Vec3::new(half, -half, 0.0),
                center + Vec3::new(half, half, 0.0),
                center + Vec3::new(-half, half, 0.0),
            ],
            rooms: [PortalSide::Room(rooms[0]), PortalSide::Room(rooms[1])],
        }
    }

    /// A doorway in a YZ plane (a side wall).
    fn side_doorway(ref_id: u32, center: Vec3, half: f32, rooms: [usize; 2]) -> Portal {
        Portal {
            ref_id,
            corners: [
                center + Vec3::new(0.0, -half, -half),
                center + Vec3::new(0.0, -half, half),
                center + Vec3::new(0.0, half, half),
                center + Vec3::new(0.0, half, -half),
            ],
            rooms: [PortalSide::Room(rooms[0]), PortalSide::Room(rooms[1])],
        }
    }

    /// A box of the given half-extents, unrotated.
    fn boxed(ref_id: u32, center: Vec3, half: Vec3) -> Room {
        Room {
            ref_id,
            center,
            rotation: Quat::IDENTITY,
            half,
        }
    }

    /// Pushes onto `rooms` a room parked far behind the camera and onto `portals` a doorway to it
    /// from each room index in `joined`, so those rooms count as rooms with a `PortalMarker` - the
    /// switch the overlap rule reads (design-1100). The parked room is never reached and never in
    /// view: its doorways are behind the camera, so they project to nothing. `joined` must not
    /// contain the parked index, which is `rooms.len()` on entry.
    fn park_portals(rooms: &mut Vec<Room>, portals: &mut Vec<Portal>, joined: &[usize]) {
        let parked = rooms.len();
        rooms.push(room(900 + parked as u32, Vec3::new(0.0, 0.0, 5000.0), 1.0));
        for (slot, &index) in joined.iter().enumerate() {
            portals.push(doorway(
                800 + slot as u32,
                Vec3::new(0.0, 0.0, 4999.0),
                1.0,
                [index, parked],
            ));
        }
    }

    /// The camera sits at the origin looking down -Z with a 60 degree square view.
    fn clip_from_local() -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_3, 1.0, 0.1)
    }

    /// Rooms A (0), B (1) and C (2): A at the camera, B behind the doorway at z = -10 and C behind
    /// a doorway in B's far wall at `far_door`. The boxes touch and `overlaps` is empty: the loader
    /// would add overlap edges for touching boxes, but these tests exercise the doorway walk alone.
    fn chain(far_door: Portal) -> RoomGraph {
        RoomGraph {
            rooms: vec![
                room(1, Vec3::ZERO, 10.0),
                room(2, Vec3::new(0.0, 0.0, -20.0), 10.0),
                room(3, Vec3::new(0.0, 0.0, -40.0), 10.0),
            ],
            portals: vec![
                doorway(10, Vec3::new(0.0, 0.0, -10.0), 3.0, [0, 1]),
                far_door,
            ],
            links: vec![],
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: vec![],
            membership: HashMap::new(),
            authored: None,
            supports_unbounded: false,
        }
    }

    #[test]
    fn a_reached_room_with_a_dropped_link_draws_the_whole_cell() {
        let connection = database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [0.0, 60.0, 0.0]);
        insert_room(&connection, 3, 8, [0.0, 120.0, 0.0]);
        connection
            .execute(
                "INSERT INTO room_portals VALUES (10,7,0,30,0,0,0,0,4,6,1,2)",
                [],
            )
            .unwrap();
        for target in [9999, 3] {
            connection.execute("DELETE FROM room_links", []).unwrap();
            connection
                .execute("INSERT INTO room_links VALUES (2,?1)", [target])
                .unwrap();
            let graph = load_room_graph(&connection, 7).unwrap();
            assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
            // An unresolved connection in an unreached room does not disable useful culling.
            connection
                .execute("UPDATE room_portals SET pos_x=1000", [])
                .unwrap();
            let graph = load_room_graph(&connection, 7).unwrap();
            assert_eq!(
                reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap(),
                vec![true, false]
            );
            connection
                .execute("UPDATE room_portals SET pos_x=0", [])
                .unwrap();
        }
    }

    #[test]
    fn a_doorway_in_view_reaches_the_room_behind_it() {
        let graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true]);
    }

    #[test]
    fn a_doorway_outside_the_frustum_leaves_its_room_culled() {
        // The doorway sits in B's side wall at 40 degrees to the right: outside a 30 degree
        // half-angle view.
        let graph = chain(side_doorway(11, Vec3::new(10.0, 0.0, -12.0), 1.5, [1, 2]));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, false]);
    }

    #[test]
    fn the_region_narrows_at_each_doorway() {
        // The far doorway is inside the screen but outside the part of it that the near doorway
        // lets through.
        let graph = chain(doorway(11, Vec3::new(12.0, 0.0, -30.0), 1.0, [1, 2]));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, false]);
        // Moved inside the narrowed region, it is reached.
        let graph = chain(doorway(11, Vec3::new(4.0, 0.0, -30.0), 1.0, [1, 2]));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true]);
    }

    #[test]
    fn a_doorway_behind_the_camera_is_not_taken() {
        let mut graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        // Turn the camera round by mirroring the scene's Z: the near doorway is now behind it.
        let around = Mat4::from_rotation_y(std::f32::consts::PI);
        graph.portals[0] = doorway(10, Vec3::new(0.0, 0.0, 10.0), 3.0, [0, 1]);
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, false, false]);
        // Looking the other way takes it.
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local() * around).unwrap();
        assert!(reached[1]);
    }

    #[test]
    fn a_link_is_an_open_edge() {
        let mut graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        // No doorway reaches C from the camera's side, but A is linked to it. A and C carry a
        // portal of their own (parked behind the camera) so the walk runs from either end - a
        // start room with none draws the whole cell now.
        graph.portals.clear();
        park_portals(&mut graph.rooms, &mut graph.portals, &[0, 2]);
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, false, false, false]);
        graph.links.push((0, 2));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, false, true, false]);
        // The link is symmetric: from C the camera's own room is reached too.
        let reached = reached_rooms(&graph, Vec3::new(0.0, 0.0, -40.0), clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, false, true, false]);
    }

    #[test]
    fn a_reached_room_with_an_unresolved_portal_side_draws_the_whole_cell() {
        let mut graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        let mut unresolved = doorway(12, Vec3::new(0.0, 0.0, -20.0), 3.0, [1, 2]);
        unresolved.rooms[1] = PortalSide::Unknown;
        graph.portals.push(unresolved);
        assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
    }

    #[test]
    fn a_camera_in_no_room_culls_nothing() {
        let graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        assert!(reached_rooms(&graph, Vec3::new(500.0, 0.0, 0.0), clip_from_local()).is_none());
    }

    fn outside_graph() -> RoomGraph {
        let mut entrance = doorway(10, Vec3::new(0.0, 0.0, -10.0), 2.0, [0, 0]);
        entrance.rooms = [PortalSide::Unbounded, PortalSide::Room(0)];
        let mut offscreen = doorway(11, Vec3::new(40.0, 0.0, -10.0), 2.0, [1, 1]);
        offscreen.rooms = [PortalSide::Unbounded, PortalSide::Room(1)];
        RoomGraph {
            authored: None,
            supports_unbounded: true,
            rooms: vec![
                room(1, Vec3::new(0.0, 0.0, -20.0), 5.0),
                room(2, Vec3::new(40.0, 0.0, -20.0), 5.0),
            ],
            portals: vec![entrance, offscreen],
            membership: HashMap::from([
                (
                    100,
                    Membership {
                        contained: vec![0],
                        overlapping: vec![],
                    },
                ),
                (
                    101,
                    Membership {
                        contained: vec![1],
                        overlapping: vec![],
                    },
                ),
                (102, Membership::default()),
            ]),
            ..default()
        }
    }

    #[test]
    fn human_review_camera_crosses_a_room_seam_without_restricting_gap_frames() {
        let mut graph = outside_graph();
        graph.rooms = vec![
            room(1, Vec3::new(-5.595, 0.0, 0.0), 5.0),
            room(2, Vec3::new(5.595, 0.0, 0.0), 5.0),
        ];
        graph.portals = vec![doorway(10, Vec3::ZERO, 2.0, [0, 1])];
        for x in [-0.7, -0.5, 0.0, 0.5, 0.7] {
            let camera = Vec3::new(x, 0.0, 0.0);
            let visible = graph
                .visibility_volumes(camera, clip_from_local() * Mat4::from_translation(-camera));
            if graph.rooms_containing(camera).is_empty() {
                assert!(
                    visible.is_none(),
                    "x={x}: a gap is not verified outside space"
                );
            } else {
                let visible =
                    visible.expect("the authored doorway still connects both bounded sides");
                assert!(visible.rooms.iter().all(RoomVisibility::is_reached));
            }
        }
    }

    #[test]
    fn human_review_negative_extents_in_old_packs_only_disable_the_affected_cell() {
        for invalid_room in [true, false] {
            let connection = database();
            insert_room(&connection, 1, 7, [0.0; 3]);
            insert_room(&connection, 2, 8, [0.0; 3]);
            if invalid_room {
                connection
                    .execute("UPDATE room_bounds SET half_x=-1 WHERE ref_id=1", [])
                    .unwrap();
            } else {
                connection
                    .execute(
                        "INSERT INTO room_portals VALUES(10,7,0,0,0,0,0,0,-4,6,1,1)",
                        [],
                    )
                    .unwrap();
            }
            let graph = load_room_graph(&connection, 7).unwrap();
            assert!(
                graph.incomplete,
                "invalid_room={invalid_room}: reject invalid source extents even without new metadata"
            );
            assert!(
                graph
                    .visibility_volumes(Vec3::ZERO, clip_from_local())
                    .is_none()
            );
            assert!(!load_room_graph(&connection, 8).unwrap().incomplete);
        }
    }

    #[test]
    fn unbounded_camera_positions_without_coverage_keep_full_rendering() {
        let graph = outside_graph();
        assert!(
            graph
                .visibility_volumes(Vec3::ZERO, clip_from_local())
                .is_none()
        );
    }

    #[test]
    fn a_room_can_see_unbounded_geometry_only_through_its_aperture() {
        let graph = outside_graph();
        let eye = Vec3::new(0.0, 0.0, -20.0);
        let clip = clip_from_local()
            * Mat4::from_rotation_y(std::f32::consts::PI)
            * Mat4::from_translation(-eye);
        let visible = graph.visibility_volumes(eye, clip).unwrap();
        let bounds_at = |x| {
            MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
                Affine3A::from_translation(Vec3::new(x, 0.0, 0.0)),
            )
            .unwrap()
        };
        assert!(visible.unbounded.is_reached());
        assert!(graph.mesh_is_drawn(102, &visible, Some(&bounds_at(0.0)), None));
        assert!(!graph.mesh_is_drawn(102, &visible, Some(&bounds_at(8.0)), None));
        let away = graph
            .visibility_volumes(eye, clip_from_local() * Mat4::from_translation(-eye))
            .unwrap();
        assert!(!away.unbounded.is_reached());
        assert!(!graph.mesh_is_drawn(102, &away, None, None));
    }

    #[test]
    fn portal_sharing_includes_the_unbounded_endpoint_volume() {
        let graph = outside_graph();
        // Test shared-geometry consumption independently of conservative camera-start policy.
        let full = VisibilityVolume::camera(clip_from_local()).unwrap();
        let mut visible = visibility_for_rooms(&[false, false]);
        assert!(visible.unbounded.insert(full.clone(), &full).is_some());
        let aggregate = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(8.0), Vec3::splat(8.0)),
            Affine3A::from_translation(Vec3::new(0.0, 0.0, -12.0)),
        )
        .unwrap();
        let near_mesh = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
            Affine3A::from_translation(Vec3::new(4.0, 0.0, -8.0)),
        )
        .unwrap();
        assert!(!graph.mesh_is_drawn(100, &visible, Some(&near_mesh), None));
        assert!(graph.mesh_is_drawn(100, &visible, Some(&near_mesh), Some(&aggregate)));
    }

    #[test]
    fn unknown_endpoints_do_not_become_unbounded_connections() {
        let mut graph = outside_graph();
        graph.portals[0].rooms[0] = PortalSide::Unknown;
        assert!(
            graph
                .visibility_volumes(Vec3::ZERO, clip_from_local())
                .is_none()
        );
        assert!(
            graph
                .visibility_volumes(Vec3::new(0.0, 0.0, -20.0), clip_from_local())
                .is_none()
        );
    }

    /// The start rule (analysis-1112): a camera in a room that has no `PortalMarker` of the cell
    /// gets `None` - the whole cell is drawn - because such rooms are open halls whose neighbours
    /// are joined by geometry the room boxes do not describe, and a walk from there can stop short
    /// of rooms plainly in view behind doorways that project off screen.
    #[test]
    fn a_camera_in_a_room_with_no_portal_draws_the_whole_cell() {
        let mut graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        assert!(
            reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_some(),
            "with a portal of its own the hall walks"
        );
        // The hall's only doorway is gone; the far doorway between B and C remains.
        graph.portals.remove(0);
        assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
    }

    /// Every room containing the camera is a start, and one start without a portal is enough: the
    /// mixed case draws the whole cell too.
    #[test]
    fn a_camera_also_in_a_portal_less_room_draws_the_whole_cell() {
        let mut rooms = vec![
            room(1, Vec3::ZERO, 10.0),
            boxed(2, Vec3::new(0.0, 0.0, -5.0), Vec3::new(10.0, 10.0, 10.0)),
        ];
        let mut portals = Vec::new();
        // Room 0 carries a portal of the cell (parked behind the camera); room 1 has none.
        park_portals(&mut rooms, &mut portals, &[0]);
        let graph = RoomGraph {
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            portals,
            rooms,
            ..Default::default()
        };
        assert_eq!(
            graph.rooms_containing(Vec3::ZERO).len(),
            2,
            "the camera stands in both boxes"
        );
        assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
    }

    /// The behaviour the start rule must not lose: a camera in a room that has a portal of the cell
    /// still walks, and rooms no in-view doorway chain leads to stay culled.
    #[test]
    fn a_camera_in_a_room_with_a_portal_still_walks_and_culls() {
        let graph = chain(side_doorway(11, Vec3::new(10.0, 0.0, -12.0), 1.5, [1, 2]));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, false]);
    }

    #[test]
    fn a_camera_in_a_doorway_sees_into_both_rooms() {
        let graph = chain(side_doorway(11, Vec3::new(10.0, 0.0, -12.0), 1.5, [1, 2]));
        // On the wall between A and B, the doorway has no usable projection.
        let reached = reached_rooms(&graph, Vec3::new(0.0, 0.0, -10.0), clip_from_local()).unwrap();
        assert!(reached[0] && reached[1]);
    }

    #[test]
    fn an_object_in_two_rooms_is_drawn_when_either_is_reached() {
        let mut graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        graph.membership.insert(
            100,
            Membership {
                contained: vec![2],
                overlapping: vec![0],
            },
        );
        graph.membership.insert(
            101,
            Membership {
                contained: vec![2],
                overlapping: vec![],
            },
        );
        graph.membership.insert(
            102,
            Membership {
                contained: vec![],
                overlapping: vec![2],
            },
        );
        graph.membership.insert(
            103,
            Membership {
                contained: vec![1, 2],
                overlapping: vec![],
            },
        );
        let reached = visibility_for_rooms(&[true, false, false]);
        assert!(
            graph.mesh_is_drawn(100, &reached, None, None),
            "overlaps a reached room"
        );
        let aggregate = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(45.0), Vec3::splat(45.0)),
            Affine3A::from_translation(Vec3::new(0.0, 0.0, -40.0)),
        )
        .unwrap();
        let near_mesh = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
            Affine3A::from_translation(Vec3::new(0.0, 0.0, -5.0)),
        )
        .unwrap();
        assert!(
            graph.mesh_is_drawn(100, &reached, Some(&near_mesh), Some(&aggregate)),
            "a conservative portal overlap must not discard other visible membership candidates"
        );
        assert!(
            !graph.mesh_is_drawn(101, &reached, None, None),
            "only in an unreached room"
        );
        assert!(
            graph.mesh_is_drawn(102, &reached, None, None),
            "pivot in no room is drawn"
        );
        assert!(
            graph.mesh_is_drawn(103, &reached, None, None),
            "pivot in several is drawn"
        );
        assert!(
            graph.mesh_is_drawn(999, &reached, None, None),
            "not in the graph is drawn"
        );
        assert!(graph.mesh_is_drawn(101, &visibility_for_rooms(&[true, false, true]), None, None));
    }

    // --- The system ---------------------------------------------------------------------------

    struct Scene {
        app: App,
        camera: Entity,
        near: Entity,
        far: Entity,
        loose: Entity,
        light: Entity,
    }

    /// A camera in room A of a two-room cell whose only doorway points away from it, so room B's
    /// objects are culled. `near` is in A, `far` in B and `loose` has no FormId at all.
    fn scene() -> Scene {
        scene_with(GlobalTransform::IDENTITY, GlobalTransform::IDENTITY)
    }

    /// [`scene`] with the cell root at `root_transform` and the camera at `camera_transform`.
    fn scene_with(root_transform: GlobalTransform, camera_transform: GlobalTransform) -> Scene {
        let mut app = App::new();
        app.add_plugins(RoomCullingPlugin {
            database_path: PathBuf::from("does-not-exist.db"),
        });
        let root = app
            .world_mut()
            .spawn((StreamedCellRoot, CellRef(7), root_transform))
            .id();
        let mut graph = RoomGraph {
            rooms: vec![
                room(1, Vec3::ZERO, 10.0),
                room(2, Vec3::new(0.0, 0.0, 20.0), 10.0),
            ],
            portals: vec![doorway(10, Vec3::new(0.0, 0.0, 10.0), 3.0, [0, 1])],
            links: vec![],
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: vec![],
            membership: HashMap::new(),
            authored: None,
            supports_unbounded: false,
        };
        graph.membership.insert(
            1,
            Membership {
                contained: vec![0],
                overlapping: vec![],
            },
        );
        graph.membership.insert(
            2,
            Membership {
                contained: vec![1],
                overlapping: vec![],
            },
        );
        app.world_mut()
            .resource_mut::<RoomCullingState>()
            .insert_graph(7, root, graph);
        let near_ref = app
            .world_mut()
            .spawn((
                FormId(1),
                ChildOf(root),
                ExpectedModelBounds::new(-Vec3::ONE, Vec3::ONE).unwrap(),
            ))
            .id();
        let near = app.world_mut().spawn(ChildOf(near_ref)).id();
        let far_ref = app
            .world_mut()
            .spawn((
                FormId(2),
                ChildOf(root),
                ExpectedModelBounds::new(-Vec3::ONE, Vec3::ONE).unwrap(),
            ))
            .id();
        let far = app.world_mut().spawn(ChildOf(far_ref)).id();
        let loose = app.world_mut().spawn(ChildOf(root)).id();
        let list = |entities: &[Entity]| {
            let mut visible = VisibleEntities::default();
            visible
                .get_mut(TypeId::of::<Mesh3d>())
                .extend_from_slice(entities);
            visible
        };
        let all = [near, far, loose];
        let camera = app
            .world_mut()
            .spawn((
                StreamingCamera,
                camera_transform,
                Projection::Perspective(PerspectiveProjection {
                    fov: std::f32::consts::FRAC_PI_3,
                    aspect_ratio: 1.0,
                    near: 0.1,
                    far: 1000.0,
                    ..default()
                }),
                list(&all),
            ))
            .id();
        // The camera looks down -Z; the doorway at +Z is behind it.
        let light = app
            .world_mut()
            .spawn((
                CubemapVisibleEntities::default(),
                VisibleMeshEntities {
                    entities: all.to_vec(),
                },
                list(&all),
            ))
            .id();
        Scene {
            app,
            camera,
            near,
            far,
            loose,
            light,
        }
    }

    fn mesh_list(app: &App, entity: Entity) -> Vec<Entity> {
        app.world()
            .get::<VisibleEntities>(entity)
            .unwrap()
            .get(TypeId::of::<Mesh3d>())
            .to_vec()
    }

    fn run_cull(app: &mut App) {
        // The system is in PostUpdate behind Bevy's visibility set; run it on its own here.
        app.world_mut()
            .run_system_once(cull_room_views)
            .expect("cull_room_views runs");
    }

    #[test]
    fn portal_shared_reference_uses_both_endpoint_volumes_for_its_meshes() {
        let mut scene = scene();
        {
            let mut state = scene.app.world_mut().resource_mut::<RoomCullingState>();
            let graph = &mut state.graphs.get_mut(&7).unwrap().graph;
            graph.rooms[1].center.z = -20.0;
            graph.portals[0] = doorway(10, Vec3::new(0.0, 0.0, -10.0), 3.0, [0, 1]);
        }
        let reference = scene
            .app
            .world()
            .get::<ChildOf>(scene.far)
            .unwrap()
            .parent();
        scene.app.world_mut().entity_mut(reference).insert((
            crate::world::components::ExpectedModelBounds::new(-Vec3::splat(8.0), Vec3::splat(8.0))
                .unwrap(),
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, -12.0)),
        ));
        scene.app.world_mut().entity_mut(scene.far).insert((
            Aabb::from_min_max(-Vec3::splat(0.25), Vec3::splat(0.25)),
            GlobalTransform::from_translation(Vec3::new(4.0, 0.0, -8.0)),
        ));
        run_cull(&mut scene.app);
        assert!(
            mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "a portal-shared reference keeps meshes visible from the near endpoint, before the aperture"
        );
    }

    #[test]
    fn a_reached_room_tests_each_mesh_against_the_portal_aperture() {
        for root in [
            GlobalTransform::IDENTITY,
            GlobalTransform::from(Transform {
                translation: Vec3::new(100.0, 50.0, -30.0),
                rotation: Quat::from_rotation_y(1.0),
                scale: Vec3::new(2.0, 1.5, 0.75),
            }),
        ] {
            let mut scene = scene_with(root, root);
            {
                let mut state = scene.app.world_mut().resource_mut::<RoomCullingState>();
                let graph = &mut state.graphs.get_mut(&7).unwrap().graph;
                graph.rooms[1].center.z = -20.0;
                graph.portals[0] = doorway(10, Vec3::new(0.0, 0.0, -10.0), 3.0, [0, 1]);
            }
            let reference = scene
                .app
                .world()
                .get::<ChildOf>(scene.far)
                .unwrap()
                .parent();
            let bounds = Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5));
            scene.app.world_mut().entity_mut(scene.far).insert((
                bounds,
                root.mul_transform(Transform::from_xyz(0.0, 0.0, -20.0)),
            ));
            let outside = scene
                .app
                .world_mut()
                .spawn((
                    ChildOf(reference),
                    bounds,
                    root.mul_transform(Transform::from_xyz(9.0, 0.0, -20.0)),
                ))
                .id();
            scene
                .app
                .world_mut()
                .get_mut::<VisibleEntities>(scene.camera)
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>())
                .push(outside);
            run_cull(&mut scene.app);
            let visible = mesh_list(&scene.app, scene.camera);
            assert!(
                visible.contains(&scene.far),
                "mesh inside the aperture stays"
            );
            assert!(
                !visible.contains(&outside),
                "another mesh of the same reference is outside the aperture"
            );
            let mut alternate_list = VisibleEntities::default();
            alternate_list
                .get_mut(TypeId::of::<Mesh3d>())
                .extend([scene.far, outside]);
            let alternate = scene
                .app
                .world_mut()
                .spawn((
                    RoomCullingView,
                    root.mul_transform(Transform::from_xyz(-4.0, 0.0, 0.0)),
                    Projection::Perspective(PerspectiveProjection {
                        fov: std::f32::consts::FRAC_PI_2,
                        aspect_ratio: 1.0,
                        ..default()
                    }),
                    alternate_list,
                ))
                .id();
            run_cull(&mut scene.app);
            assert!(
                mesh_list(&scene.app, alternate).contains(&outside),
                "a different camera sees this mesh through its own portal volume"
            );
            assert!(!mesh_list(&scene.app, scene.camera).contains(&outside));
            scene
                .app
                .world_mut()
                .entity_mut(outside)
                .insert(NoFrustumCulling);
            scene
                .app
                .world_mut()
                .get_mut::<VisibleEntities>(scene.camera)
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>())
                .push(outside);
            run_cull(&mut scene.app);
            assert!(
                mesh_list(&scene.app, scene.camera).contains(&outside),
                "explicit frustum opt-outs stay visible"
            );
        }
    }

    #[test]
    fn room_volumes_keep_alternative_paths_and_linked_rooms() {
        let mut graph = RoomGraph {
            rooms: vec![
                room(1, Vec3::ZERO, 5.0),
                room(2, Vec3::new(0.0, 0.0, -20.0), 10.0),
            ],
            portals: vec![
                doorway(10, Vec3::new(-3.0, 0.0, -10.0), 1.0, [0, 1]),
                doorway(11, Vec3::new(3.0, 0.0, -10.0), 1.0, [0, 1]),
            ],
            membership: HashMap::from([(
                100,
                Membership {
                    contained: vec![1],
                    overlapping: vec![],
                },
            )]),
            ..default()
        };
        let bounds_at = |x| {
            MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
                Affine3A::from_translation(Vec3::new(x, 0.0, -20.0)),
            )
            .unwrap()
        };
        let volumes = graph
            .visibility_volumes(Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(graph.mesh_is_drawn(100, &volumes, Some(&bounds_at(-6.0)), None));
        assert!(graph.mesh_is_drawn(100, &volumes, Some(&bounds_at(6.0)), None));
        assert!(
            !graph.mesh_is_drawn(100, &volumes, Some(&bounds_at(0.0)), None),
            "the gap between paths stays hidden"
        );
        graph.links.push((0, 1));
        let volumes = graph
            .visibility_volumes(Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(
            graph.mesh_is_drawn(100, &volumes, Some(&bounds_at(0.0)), None),
            "an explicit link passes the full starting volume"
        );
    }

    #[test]
    fn missing_stored_bounds_keep_a_floor_that_extends_out_of_its_pivot_room() {
        let mut scene = scene();
        let reference = scene
            .app
            .world()
            .get::<ChildOf>(scene.far)
            .unwrap()
            .parent();
        // The rear pivot has no overlap rows because the converted model bounds were invalid.
        // Its loaded mesh still extends through the camera's room into the forward view.
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .remove::<ExpectedModelBounds>()
            .insert(GlobalTransform::from_translation(Vec3::new(0.0, 0.0, 20.0)));
        scene.app.world_mut().entity_mut(scene.far).insert((
            Aabb::from_min_max(Vec3::new(-5.0, -0.1, -35.0), Vec3::new(5.0, 0.1, 1.0)),
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, 20.0)),
        ));
        run_cull(&mut scene.app);
        assert!(
            mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "a runtime mesh AABB does not make pivot-only stored membership complete"
        );
        assert_eq!(
            scene
                .app
                .world()
                .resource::<RoomCullingState>()
                .culled_last_frame,
            0
        );
    }

    #[test]
    fn room_culling_setting_changes_the_next_frame_and_keeps_capture_counts() {
        let mut scene = scene();
        for enabled in [true, false, true] {
            let settings: crate::settings::RenderingSettings =
                serde_json::from_value(serde_json::json!({"room_culling": enabled})).unwrap();
            scene.app.insert_resource(settings);
            *scene
                .app
                .world_mut()
                .get_mut::<VisibleEntities>(scene.camera)
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>()) = vec![scene.near, scene.far, scene.loose];
            run_cull(&mut scene.app);
            let expected = if enabled {
                vec![scene.near, scene.loose]
            } else {
                vec![scene.near, scene.far, scene.loose]
            };
            assert_eq!(
                mesh_list(&scene.app, scene.camera),
                expected,
                "room_culling={enabled}"
            );
            let state = scene.app.world().resource::<RoomCullingState>();
            assert_eq!(
                state.main_view_meshes,
                Some(MainViewMeshes {
                    listed: 3,
                    drawn: expected.len()
                })
            );
            assert_eq!(state.culled_last_frame, 3 - expected.len());
        }
    }

    #[test]
    fn the_main_view_loses_unreached_objects_and_shadows_keep_them() {
        let mut scene = scene();
        run_cull(&mut scene.app);
        assert_eq!(
            mesh_list(&scene.app, scene.camera),
            vec![scene.near, scene.loose],
            "the object in the unreached room is gone from the main view, the loose one stays"
        );
        // A light's own sets, and another camera's list, are untouched.
        let light = scene.app.world().entity(scene.light);
        assert_eq!(
            light.get::<VisibleMeshEntities>().unwrap().entities,
            vec![scene.near, scene.far, scene.loose]
        );
        assert_eq!(
            mesh_list(&scene.app, scene.light),
            vec![scene.near, scene.far, scene.loose],
            "a view that did not opt in keeps its list"
        );
        assert_eq!(
            scene
                .app
                .world()
                .resource::<RoomCullingState>()
                .culled_last_frame,
            1
        );
    }

    #[test]
    fn a_camera_that_opts_in_is_culled_too() {
        let mut scene = scene();
        scene.app.world_mut().entity_mut(scene.light).insert((
            RoomCullingView,
            GlobalTransform::IDENTITY,
            Projection::default(),
        ));
        run_cull(&mut scene.app);
        assert!(!mesh_list(&scene.app, scene.light).contains(&scene.far));
        assert!(
            scene
                .app
                .world()
                .entity(scene.light)
                .get::<VisibleMeshEntities>()
                .unwrap()
                .entities
                .contains(&scene.far),
            "even then its shadow casters are its own"
        );
    }

    /// Two resident cells may both contain the camera during a transition. Each graph must
    /// filter its own references, including when the other cell has a lower FormID.
    #[test]
    fn every_resident_interior_is_culled() {
        let mut scene = scene();
        let root = scene
            .app
            .world_mut()
            .spawn((StreamedCellRoot, CellRef(8), GlobalTransform::IDENTITY))
            .id();
        let mut graph = scene.app.world().resource::<RoomCullingState>().graphs[&7]
            .graph
            .clone();
        graph.membership = graph
            .membership
            .into_iter()
            .map(|(id, membership)| (id + 100, membership))
            .collect();
        scene
            .app
            .world_mut()
            .resource_mut::<RoomCullingState>()
            .insert_graph(8, root, graph);
        let mut meshes = Vec::new();
        for id in [101, 102] {
            let reference = scene
                .app
                .world_mut()
                .spawn((
                    FormId(id),
                    ChildOf(root),
                    ExpectedModelBounds::new(-Vec3::ONE, Vec3::ONE).unwrap(),
                ))
                .id();
            meshes.push(scene.app.world_mut().spawn(ChildOf(reference)).id());
        }
        scene
            .app
            .world_mut()
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .extend_from_slice(&meshes);
        run_cull(&mut scene.app);
        assert_eq!(
            mesh_list(&scene.app, scene.camera),
            vec![scene.near, scene.loose, meshes[0]],
            "each cell loses its far mesh and keeps its near mesh"
        );
        assert_eq!(
            scene
                .app
                .world()
                .resource::<RoomCullingState>()
                .main_view_meshes,
            Some(MainViewMeshes {
                listed: 5,
                drawn: 3
            })
        );
    }

    #[test]
    fn a_camera_in_no_room_keeps_every_object() {
        let mut scene = scene();
        scene
            .app
            .world_mut()
            .entity_mut(scene.camera)
            .insert(GlobalTransform::from_xyz(900.0, 0.0, 0.0));
        run_cull(&mut scene.app);
        assert_eq!(
            mesh_list(&scene.app, scene.camera),
            vec![scene.near, scene.far, scene.loose]
        );
    }

    #[test]
    fn replacing_an_interior_root_requests_its_graph_before_capture_readiness() {
        AsyncComputeTaskPool::get_or_init(Default::default);
        let mut scene = scene();
        let old = scene.app.world().resource::<RoomCullingState>().graphs[&7].root;
        scene.app.world_mut().despawn(old);
        let replacement = scene
            .app
            .world_mut()
            .spawn((StreamedCellRoot, CellRef(7)))
            .id();
        scene.app.world_mut().run_schedule(Update);
        let state = scene.app.world().resource::<RoomCullingState>();
        assert!(!state.graphs.contains_key(&7));
        assert_eq!(
            state.pending_reads(),
            1,
            "the replacement must hold the capture gate"
        );
        assert_eq!(state.failed_reads(), 0);
        assert!(scene.app.world().get_entity(replacement).is_ok());
    }

    #[test]
    fn a_read_of_a_missing_database_fails_once_and_leaves_the_view_alone() {
        AsyncComputeTaskPool::get_or_init(Default::default);
        let mut scene = scene();
        // No graph exists for cell 8; asking for it reads the (missing) database.
        scene.app.world_mut().spawn((StreamedCellRoot, CellRef(8)));
        for _ in 0..500 {
            scene.app.update();
            let state = scene.app.world().resource::<RoomCullingState>();
            if state.failed.contains(&8) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let state = scene.app.world().resource::<RoomCullingState>();
        assert!(state.failed.contains(&8) && state.pending.is_empty());
        assert!(state.active(), "a failed read is not an absent table");
        // Several more frames do not queue another read of the failed cell.
        scene.app.update();
        scene.app.update();
        let state = scene.app.world().resource::<RoomCullingState>();
        assert!(state.pending.is_empty());
    }

    // --- Round-1 review: the real paths -------------------------------------------------------

    /// A file named like the engine's own, holding the section 15 tables beside the tables the
    /// loader reads. Reference 500 sits on base object 20; reference 501 sits on base object 500,
    /// which is another placement's id, so a lookup by the base id would pick the wrong room.
    fn write_world_fixture(directory: &std::path::Path) -> PathBuf {
        let path = directory.join("skyrim_world.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER NOT NULL);
                 INSERT INTO schema_info VALUES({});
                 CREATE TABLE statics(id INTEGER PRIMARY KEY,model_path TEXT,bounds_min_x REAL,bounds_min_y REAL,bounds_min_z REAL,bounds_max_x REAL,bounds_max_y REAL,bounds_max_z REAL,bounds_valid INTEGER NOT NULL,flags INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE \"references\"(id INTEGER PRIMARY KEY,cell_id INTEGER,base_form_id INTEGER,pos_x REAL,pos_y REAL,pos_z REAL,rot_x REAL,rot_y REAL,rot_z REAL,scale REAL);
                 INSERT INTO statics VALUES(20,'architecture/wall.nif',-1,-2,-3,1,2,3,1,0);
                 INSERT INTO statics VALUES(500,'architecture/floor.nif',-1,-2,-3,1,2,3,1,0);
                 INSERT INTO \"references\" VALUES(500,7,20,0,0,0,0,0,0,1);
                 INSERT INTO \"references\" VALUES(501,7,500,1000,0,0,0,0,0,1);",
                shared::WORLD_DATABASE_SCHEMA_VERSION
            ))
            .unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        // Rooms 1 and 2 side by side, no doorway and no link: from room 1 only room 1 is reached.
        insert_room(&connection, 1, 7, [0.0, 0.0, 0.0]);
        insert_room(&connection, 2, 7, [1000.0, 0.0, 0.0]);
        connection
            .execute("INSERT INTO reference_rooms VALUES (500,1,1),(501,2,1)", [])
            .unwrap();
        path
    }

    /// The whole key chain through the real loader: `WorldDatabase` delivers `references.id` as
    /// `ReferenceRow::form_id` (which `streaming::spawn_cell` puts on the entity as `FormId`), and
    /// `reference_rooms.reference_id` is that same number.
    #[test]
    fn the_membership_key_is_the_reference_id_the_loader_delivers() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_world_fixture(directory.path());
        // Room 1 carries a portal of the cell - a doorway to a room parked behind the camera - so
        // the walk runs: a start room with no portal draws the whole cell now. The parked doorway
        // is behind the lens and never taken, so room 2 stays unreachable through the graph.
        {
            let connection = Connection::open(&path).unwrap();
            insert_room(&connection, 3, 7, [0.0, -5000.0, 0.0]);
            connection
                .execute(
                    "INSERT INTO room_portals VALUES (10,7,0,-4999,0,0,0,0,4,6,1,3)",
                    [],
                )
                .unwrap();
        }
        let database = WorldDatabase::open(&path).unwrap();
        database
            .request(DatabaseRequest::Load {
                generation: 1,
                key: CellKey::Interior(7),
                queued_at: std::time::Instant::now(),
            })
            .unwrap();
        let response = loop {
            if let Some(response) = database.try_response() {
                break response;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let mut references = response.result.expect("the cell loads").references;
        references.sort_by_key(|reference| reference.form_id);
        let keys: Vec<(u32, u32)> = references
            .iter()
            .map(|reference| (reference.form_id, reference.base_form_id))
            .collect();
        assert_eq!(
            keys,
            vec![(500, 20), (501, 500)],
            "form_id is references.id"
        );

        let graph = read_graph(&path, 7).unwrap().expect("the tables are there");
        assert_eq!(graph.rooms_containing(Vec3::ZERO), vec![0]);
        let reached = graph
            .visibility_volumes(Vec3::ZERO, clip_from_local())
            .expect("the camera is in room 1, which carries a portal");
        assert_eq!(
            reached
                .rooms
                .iter()
                .map(RoomVisibility::is_reached)
                .collect::<Vec<_>>(),
            vec![true, false, false]
        );
        for reference in &references {
            // The key the engine uses is the delivered form_id; the base id would hit the wrong row.
            assert_eq!(
                graph.mesh_is_drawn(reference.form_id, &reached, None, None),
                reference.form_id == 500
            );
        }
        assert!(
            graph.mesh_is_drawn(references[1].base_form_id, &reached, None, None),
            "base id 500 would have made reference 501 look like reference 500"
        );
    }

    /// The production read path, from a file: the path the plugin opens is the one the engine opens
    /// (`app.rs`: `config.assets_dir.join("skyrim_world.db")`, lines 96 and 113 at this commit), the
    /// tables read back, and a file without them is `None` rather than an error.
    #[test]
    fn a_world_database_file_is_read_through_the_production_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_world_fixture(directory.path());
        let config = EngineConfig {
            assets_dir: directory.path().to_path_buf(),
            ..default()
        };
        let plugin = RoomCullingPlugin::from_config(&config);
        assert_eq!(plugin.database_path, path);
        let graph = read_graph(&plugin.database_path, 7).unwrap().unwrap();
        assert_eq!((graph.rooms.len(), graph.membership.len()), (2, 2));

        let bare = directory.path().join("bare.db");
        Connection::open(&bare)
            .unwrap()
            .execute_batch("CREATE TABLE schema_info(version INTEGER);")
            .unwrap();
        assert!(read_graph(&bare, 7).unwrap().is_none());
    }

    /// Bevy's own visibility plugin, the real plugin and `app.update()`: the system has to run in
    /// `CheckVisibility` after `check_visibility_cpu_culling`, or Bevy's refill puts the culled mesh
    /// back. Room C has no doorway, so its mesh is in the frustum but must leave the camera's list
    /// while its `ViewVisibility`, which shadow passes read, stays set.
    #[test]
    fn the_real_visibility_schedule_culls_after_bevys_own_check() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            TransformPlugin,
            VisibilityPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>()
        .add_plugins(RoomCullingPlugin {
            database_path: PathBuf::from("does-not-exist.db"),
        });
        let root = app
            .world_mut()
            .spawn((
                StreamedCellRoot,
                CellRef(7),
                Transform::IDENTITY,
                Visibility::default(),
            ))
            .id();
        let graph = RoomGraph {
            authored: None,
            supports_unbounded: false,
            rooms: vec![
                room(1, Vec3::ZERO, 10.0),
                room(2, Vec3::new(0.0, 0.0, -20.0), 10.0),
                room(3, Vec3::new(0.0, 0.0, -40.0), 10.0),
            ],
            portals: vec![doorway(10, Vec3::new(0.0, 0.0, -10.0), 3.0, [0, 1])],
            links: vec![],
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: vec![],
            membership: (0..3)
                .map(|room| {
                    (
                        room as u32 + 1,
                        Membership {
                            contained: vec![room],
                            overlapping: vec![],
                        },
                    )
                })
                .collect(),
        };
        app.world_mut()
            .resource_mut::<RoomCullingState>()
            .insert_graph(7, root, graph);
        let mesh_in = |app: &mut App, form_id: u32, z: f32| {
            let reference = app
                .world_mut()
                .spawn((
                    FormId(form_id),
                    Transform::IDENTITY,
                    Visibility::default(),
                    ChildOf(root),
                    ExpectedModelBounds::new(
                        Vec3::new(-1.0, -1.0, z - 1.0),
                        Vec3::new(1.0, 1.0, z + 1.0),
                    )
                    .unwrap(),
                ))
                .id();
            app.world_mut()
                .spawn((
                    Mesh3d(Handle::default()),
                    Aabb::from_min_max(Vec3::splat(-1.0), Vec3::splat(1.0)),
                    Transform::from_xyz(0.0, 0.0, z),
                    ChildOf(reference),
                ))
                .id()
        };
        let (a, b, c) = (
            mesh_in(&mut app, 1, -3.0),
            mesh_in(&mut app, 2, -20.0),
            mesh_in(&mut app, 3, -40.0),
        );
        let outside = mesh_in(&mut app, 2, -20.0);
        app.world_mut()
            .get_mut::<Transform>(outside)
            .unwrap()
            .translation
            .x = 9.0;
        let projection = Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_3,
            aspect_ratio: 1.0,
            ..default()
        });
        let frustum = Frustum(ViewFrustum::from_clip_from_world(
            &projection.get_clip_from_view(),
        ));
        let camera = app
            .world_mut()
            .spawn((
                Camera::default(),
                StreamingCamera,
                Transform::IDENTITY,
                projection,
                frustum,
            ))
            .id();
        app.update();
        assert_eq!(
            app.world()
                .get::<GlobalTransform>(b)
                .unwrap()
                .translation()
                .z,
            -20.0,
            "the reference hierarchy propagates the mesh placement before bounds testing"
        );
        let visible = mesh_list(&app, camera);
        assert!(visible.contains(&a) && visible.contains(&b));
        assert!(
            !visible.contains(&outside),
            "a mesh in reached room B is outside the aperture"
        );
        assert!(
            app.world().get::<ViewVisibility>(outside).unwrap().get(),
            "the aperture cull follows Bevy visibility and leaves other passes intact"
        );
        assert!(
            !visible.contains(&c),
            "room C's mesh is in the frustum but unreached, so it leaves the main view"
        );
        assert!(
            app.world().get::<ViewVisibility>(c).unwrap().get(),
            "Bevy's own check still marks it visible, which is what shadow passes use"
        );
        let state = app.world().resource::<RoomCullingState>();
        assert_eq!(state.culled_last_frame, 2);
        assert_eq!(
            state.main_view_meshes,
            Some(MainViewMeshes {
                listed: visible.len() + 2,
                drawn: visible.len(),
            }),
            "the receipt counts are the main view's list before and after the cull"
        );
    }

    /// A shot does not settle on a view whose room graph is unread: it would photograph and
    /// measure the cell drawn whole. A harness without the resource adds nothing to the gate.
    #[test]
    fn unread_room_graphs_hold_the_shot_settle_gate() {
        let quiet = crate::shots::SettleCounts {
            renderer_ready: true,
            pending_specializations: Some(0),
            pending_render_transfers: Some(0),
            ..default()
        };
        assert!(quiet.with_room_graphs(None).is_quiet());
        let mut state = RoomCullingState::new(PathBuf::new());
        assert!(quiet.with_room_graphs(Some(&state)).is_quiet());
        AsyncComputeTaskPool::get_or_init(Default::default);
        state.pending.insert(
            7,
            AsyncComputeTaskPool::get().spawn(future::pending::<LoadResult>()),
        );
        let pending = quiet.with_room_graphs(Some(&state));
        assert_eq!(pending.pending_room_graphs, 1);
        assert!(!pending.is_quiet(), "a pending read must hold the capture");
        state.pending.clear();
        state.failed.insert(7);
        let failed = quiet.with_room_graphs(Some(&state));
        assert_eq!(failed.failed_room_graphs, 1);
        assert!(
            !failed.is_quiet(),
            "a cell drawn whole after a failed read is not the culled view"
        );
    }

    /// The committed measurement poses stay a valid shots file of interior poses, so the counts
    /// in `scripts/room_culling_counts.py` can be reproduced with `--shots` on any conversion.
    #[test]
    fn measurement_poses_are_a_valid_interior_shots_file() {
        let file = crate::shots::ShotsFile::parse(include_str!(
            "../tests/fixtures/room-culling-shots.json"
        ))
        .expect("the room-culling poses parse");
        assert!(!file.shots.is_empty());
        assert!(
            file.shots
                .iter()
                .all(|shot| shot.interior_cell_id.is_some()),
            "room culling only acts inside interiors"
        );
    }

    /// The A-B-C cull again with the cell root rotated and moved: the graph is root-local, so a
    /// camera at the same place in the root's frame gets the same answer, and a camera that is not
    /// in the root's frame is in no room.
    #[test]
    fn a_rotated_and_moved_cell_root_gives_the_same_answer() {
        let root = GlobalTransform::from(Transform {
            translation: Vec3::new(100.0, 5.0, -30.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            ..default()
        });
        let mut inside = scene_with(root, root);
        run_cull(&mut inside.app);
        assert_eq!(
            mesh_list(&inside.app, inside.camera),
            vec![inside.near, inside.loose]
        );
        let mut outside = scene_with(root, GlobalTransform::IDENTITY);
        run_cull(&mut outside.app);
        assert_eq!(
            mesh_list(&outside.app, outside.camera),
            vec![outside.near, outside.far, outside.loose],
            "the world origin is not inside any room of this root"
        );
    }

    /// A doorway the camera is next to counts as open even when it points away from the view; one
    /// just beyond `PORTAL_OPEN_DISTANCE` does not.
    #[test]
    fn a_doorway_next_to_the_camera_is_open_but_one_further_away_is_not() {
        let graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        // The camera looks along +Z (away from the doorway at z = -10).
        let clip_for = |position: Vec3| {
            clip_from_local()
                * Mat4::from_rotation_y(std::f32::consts::PI)
                * Mat4::from_translation(-position)
        };
        let near = Vec3::new(0.0, 0.0, -10.0 + PORTAL_OPEN_DISTANCE - 1.0);
        let reached = reached_rooms(&graph, near, clip_for(near)).unwrap();
        assert!(reached[1], "within the open distance the doorway is taken");
        let far = Vec3::new(0.0, 0.0, -10.0 + PORTAL_OPEN_DISTANCE + 1.0);
        let reached = reached_rooms(&graph, far, clip_for(far)).unwrap();
        assert!(!reached[1], "beyond it, a doorway behind the camera is not");
    }

    #[test]
    fn actors_doors_and_moving_bodies_are_never_culled_by_their_room() {
        let mut scene = scene();
        let root = scene
            .app
            .world_mut()
            .query_filtered::<Entity, With<StreamedCellRoot>>()
            .single(scene.app.world())
            .unwrap();
        // References 3..=6 are all placed in room B, which the camera cannot reach.
        let add = |app: &mut App, form_id: u32, reference: &dyn Fn(&mut EntityWorldMut)| {
            let mut reference_entity = app.world_mut().spawn((
                FormId(form_id),
                ChildOf(root),
                ExpectedModelBounds::new(-Vec3::ONE, Vec3::ONE).unwrap(),
            ));
            reference(&mut reference_entity);
            let reference_entity = reference_entity.id();
            let mesh = app.world_mut().spawn(ChildOf(reference_entity)).id();
            app.world_mut()
                .resource_mut::<RoomCullingState>()
                .graphs
                .get_mut(&7)
                .unwrap()
                .graph
                .membership
                .insert(
                    form_id,
                    Membership {
                        contained: vec![1],
                        overlapping: vec![],
                    },
                );
            mesh
        };
        let actor = add(&mut scene.app, 3, &|entity| {
            entity.insert(RoomCullExempt);
        });
        let dynamic = add(&mut scene.app, 4, &|_| {});
        scene
            .app
            .world_mut()
            .entity_mut(dynamic)
            .insert(RigidBody::Dynamic);
        let door = add(&mut scene.app, 5, &|entity| {
            entity.insert(RigidBody::KinematicPositionBased);
        });
        let fixed = add(&mut scene.app, 6, &|entity| {
            entity.insert(RigidBody::Fixed);
        });
        scene
            .app
            .world_mut()
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .extend([actor, dynamic, door, fixed]);
        run_cull(&mut scene.app);
        let visible = mesh_list(&scene.app, scene.camera);
        for kept in [actor, dynamic, door] {
            assert!(visible.contains(&kept), "{kept:?} must stay drawn");
        }
        assert!(
            !visible.contains(&fixed),
            "a fixed object in room B is culled"
        );
        assert!(!visible.contains(&scene.far), "so is the plain static");
    }

    #[test]
    fn a_room_with_a_non_finite_box_is_joined_to_its_neighbour() {
        let finite = Room {
            ref_id: 1,
            center: Vec3::ZERO,
            half: Vec3::splat(10.0),
            rotation: Quat::IDENTITY,
        };
        let broken = Room {
            center: Vec3::new(f32::NAN, 0.0, 0.0),
            ..finite.clone()
        };
        assert!(rooms_overlap(&finite, &broken));
        assert!(rooms_overlap(&broken, &finite));
    }

    #[test]
    fn actor_door_and_loose_item_records_are_exempt_and_statics_are_not() {
        for kept in ["NPC_", "LVLN", "LVLI", "DOOR", "ACTI", "MISC", "WEAP"] {
            assert!(exempt_record_type(Some(kept)), "{kept}");
        }
        for culled in ["STAT", "TREE", "FURN", "LIGH"] {
            assert!(!exempt_record_type(Some(culled)), "{culled}");
        }
        assert!(!exempt_record_type(None));
    }

    fn rotated_room(ref_id: u32, center: Vec3, half: f32, yaw: f32) -> Room {
        Room {
            rotation: Quat::from_rotation_y(yaw),
            ..room(ref_id, center, half)
        }
    }

    /// Rooms A (camera) and B overlap with no doorway and no link between them; C sits behind B,
    /// joined to B by a doorway. The overlap is the only way from A to B. A carries a portal of its
    /// own (parked behind the camera) so the walk runs - a start room with none draws the whole
    /// cell now.
    #[test]
    fn overlapping_rooms_without_a_doorway_are_one_space() {
        let mut rooms = vec![
            room(1, Vec3::ZERO, 10.0),
            room(2, Vec3::new(0.0, 0.0, -15.0), 10.0),
            room(3, Vec3::new(0.0, 0.0, -45.0), 10.0),
        ];
        let mut portals = vec![doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2])];
        park_portals(&mut rooms, &mut portals, &[0]);
        let mut graph = RoomGraph {
            portals,
            links: vec![],
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: vec![],
            membership: HashMap::new(),
            authored: None,
            supports_unbounded: false,
            rooms: rooms.clone(),
        };
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(
            reached,
            vec![true, false, false, false],
            "no overlap yet: culled"
        );
        assert_eq!(overlap_links(&rooms), vec![(0, 1)]);
        graph.overlaps = overlap_links(&rooms);
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true, false]);
    }

    /// The decision (an epsilon of 8 units): boxes that touch, or lie within the tolerance,
    /// are one space; boxes further apart are not.
    #[test]
    fn rooms_that_touch_or_nearly_touch_are_joined() {
        let a = room(1, Vec3::ZERO, 10.0);
        let touching = room(2, Vec3::new(0.0, 0.0, -20.0), 10.0);
        let near = room(
            3,
            Vec3::new(0.0, 0.0, -20.0 - OVERLAP_TOLERANCE * 0.9),
            10.0,
        );
        let deeper = room(4, Vec3::new(0.0, 0.0, -18.0), 10.0);
        let apart = room(
            5,
            Vec3::new(0.0, 0.0, -20.0 - OVERLAP_TOLERANCE * 1.1),
            10.0,
        );
        assert!(rooms_overlap(&a, &touching));
        assert!(rooms_overlap(&a, &near));
        assert!(rooms_overlap(&a, &deeper));
        assert!(!rooms_overlap(&a, &apart));
    }

    /// A diamond (a box turned 45 degrees) beside a square: the corners do not meet, and moving the
    /// diamond in makes them overlap.
    #[test]
    fn overlap_uses_the_oriented_boxes() {
        let square = room(1, Vec3::ZERO, 10.0);
        let turned = std::f32::consts::FRAC_PI_4;
        let beside = rotated_room(2, Vec3::new(40.0, 0.0, 20.0), 10.0, turned);
        assert!(!rooms_overlap(&square, &beside), "the corners do not meet");
        assert!(!rooms_overlap(&beside, &square), "and it is symmetric");
        let nearer = rotated_room(3, Vec3::new(22.0, 0.0, 0.0), 10.0, turned);
        assert!(
            rooms_overlap(&square, &nearer),
            "the diamond's tip is inside"
        );
        assert!(rooms_overlap(&nearer, &square));
        // A small turned box inside the square.
        let inner = rotated_room(4, Vec3::new(1.0, 0.0, 1.0), 3.0, 0.3);
        assert!(rooms_overlap(&square, &inner));
        assert!(rooms_overlap(&inner, &square));
    }

    /// A portal-only chain whose rooms are well apart (walls between them) has no implicit edges,
    /// so the portal walk is exactly what it was.
    #[test]
    fn a_portal_only_chain_has_no_implicit_edges() {
        let mut graph = chain(side_doorway(11, Vec3::new(10.0, 0.0, -12.0), 1.5, [1, 2]));
        // Move the rooms clearly further apart than the tolerance.
        for (index, room) in graph.rooms.iter_mut().enumerate() {
            room.center.z -= 2.0 * OVERLAP_TOLERANCE * index as f32;
        }
        assert!(overlap_links(&graph.rooms).is_empty());
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, false]);
    }

    /// Real data, Ansilvund01: yaw-rotated room 252467 (rot_z 5.4978 rad = -45 degrees) and a point
    /// of room 252468 that a wrong yaw sign puts inside it. Built the way the loader builds rooms,
    /// from Creation values through the shared conversion; the point must be OUTSIDE, and the two
    /// rooms (about 117 units apart) are not one space.
    #[test]
    fn a_yaw_rotated_room_uses_the_converters_rotation_sign() {
        let rotated = Room::from_creation(
            252467,
            [1352.5667, 1928.5657, -935.5759],
            [0.0, 0.0, 5.497_787_5],
            [528.0, 1088.0, 640.0],
        );
        let point = Vec3::from_array(shared::coordinates::creation_to_runtime_vector([
            560.0, 944.0, -336.0,
        ]));
        assert!(!rotated.contains(point), "560, 944, -336 is outside 252467");
        let neighbour = Room::from_creation(
            252468,
            [560.0, 944.0, -336.0],
            [0.0, 0.0, 0.0],
            [400.0, 464.0, 432.0],
        );
        assert!(neighbour.contains(point));
        assert!(!rooms_overlap(&rotated, &neighbour));
        // The opposite sign would put them together.
        let mirrored = Room::from_creation(
            252467,
            [1352.5667, 1928.5657, -935.5759],
            [0.0, 0.0, -5.497_787_5],
            [528.0, 1088.0, 640.0],
        );
        assert!(rooms_overlap(&mirrored, &neighbour));
    }

    /// A (camera) sees B through a narrow doorway; C overlaps B. The overlap is walked only when C's
    /// box is on screen inside the region the doorway leaves.
    fn overlap_scene(c_center: Vec3) -> RoomGraph {
        let mut rooms = vec![
            room(1, Vec3::ZERO, 10.0),
            room(2, Vec3::new(0.0, 0.0, -30.0), 10.0),
            room(3, c_center, 12.0),
        ];
        let mut portals = vec![doorway(10, Vec3::new(0.0, 0.0, -10.0), 1.0, [0, 1])];
        // C carries a portal of its own, so the B-C overlap edge stays thick: an edge with a
        // portal-less room is walked open now, and C would be reached whatever the doorway left.
        park_portals(&mut rooms, &mut portals, &[2]);
        RoomGraph {
            portals,
            links: vec![],
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            membership: HashMap::new(),
            authored: None,
            supports_unbounded: false,
            rooms,
        }
    }

    #[test]
    fn an_overlapping_room_outside_the_region_is_not_reached() {
        // C is beside B and overlaps it, but lies wholly to the right of what the doorway shows.
        let graph = overlap_scene(Vec3::new(22.0, 0.0, -34.0));
        assert!(graph.overlaps.contains(&(1, 2)), "B and C overlap");
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, false, false]);
    }

    #[test]
    fn an_overlapping_room_partly_inside_the_region_is_reached() {
        let graph = overlap_scene(Vec3::new(4.0, 0.0, -34.0));
        assert!(graph.overlaps.contains(&(1, 2)));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true, false]);
    }

    #[test]
    fn an_overlapping_room_behind_the_camera_is_not_reached() {
        let mut rooms = vec![
            room(1, Vec3::ZERO, 10.0),
            room(2, Vec3::new(0.0, 0.0, 15.0), 10.0),
        ];
        let mut portals = Vec::new();
        // Both rooms carry a portal, so the overlap edge stays thick. An edge with a portal-less
        // room is walked open (the whole screen passes), which would reach the room behind.
        park_portals(&mut rooms, &mut portals, &[0, 1]);
        let graph = RoomGraph {
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            portals,
            rooms,
            ..Default::default()
        };
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        // Room 2 spans z 5..25, behind a camera that looks down -Z.
        assert_eq!(reached, vec![true, false, false]);
    }

    /// Rooms from a list as a graph whose edges are the implicit overlaps, with every room carrying
    /// a portal (a doorway to a room parked behind the camera) so the overlaps keep the
    /// thick-portal narrowing these fixtures test.
    fn overlap_only(rooms: Vec<Room>) -> RoomGraph {
        let mut rooms = rooms;
        let mut portals = Vec::new();
        let marked: Vec<usize> = (0..rooms.len()).collect();
        park_portals(&mut rooms, &mut portals, &marked);
        RoomGraph {
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            portals,
            rooms,
            ..Default::default()
        }
    }

    fn small(ref_id: u32, center: Vec3, half: f32) -> Room {
        room(ref_id, center, half)
    }

    /// A (camera) overlaps B; C lies 6 units beside B, so B and C are joined, but C's box on screen
    /// is wholly outside B's. Crossing A-B narrows the region to B's rectangle, so C is not reached
    /// through B.
    #[test]
    fn a_thick_portal_narrows_the_region_to_both_boxes() {
        let graph = overlap_only(vec![
            small(1, Vec3::ZERO, 6.0),
            small(2, Vec3::new(0.0, 0.0, -18.0), 5.0),
            small(3, Vec3::new(16.0, 0.0, -22.0), 5.0),
        ]);
        assert!(graph.overlaps.contains(&(0, 1)) && graph.overlaps.contains(&(1, 2)));
        assert!(!graph.overlaps.contains(&(0, 2)));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, false, false]);
    }

    /// The real-data pillar case: the far room C is seen through the space it shares with B, so it
    /// is reached.
    #[test]
    fn a_room_seen_through_the_shared_volume_is_reached() {
        let graph = overlap_only(vec![
            small(1, Vec3::ZERO, 6.0),
            small(2, Vec3::new(0.0, 0.0, -18.0), 5.0),
            small(3, Vec3::new(8.0, 0.0, -22.0), 5.0),
        ]);
        assert!(graph.overlaps.contains(&(1, 2)));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true, false]);
    }

    /// A portal-bearing start reaches an open corridor, then looks across unboxed space at a
    /// backdrop whose connecting portal is below the view. The incomplete graph must not hide it.
    #[test]
    fn a_reached_portal_less_corridor_draws_the_whole_cell() {
        let mut rooms = vec![
            boxed(1, Vec3::ZERO, Vec3::new(1683.0, 928.0, 3488.0)),
            boxed(
                2,
                Vec3::new(-1500.0, 0.0, -3588.0),
                Vec3::new(1000.0, 928.0, 100.0),
            ),
            boxed(
                3,
                Vec3::new(-3300.0, 0.0, -4300.0),
                Vec3::new(900.0, 928.0, 1200.0),
            ),
            boxed(
                4,
                Vec3::new(1500.0, 0.0, -4600.0),
                Vec3::new(800.0, 928.0, 400.0),
            ),
        ];
        let mut portals = vec![doorway(20, Vec3::new(0.0, -3000.0, -4000.0), 150.0, [2, 3])];
        park_portals(&mut rooms, &mut portals, &[0]);
        let graph = RoomGraph {
            rooms: rooms.clone(),
            portals,
            links: vec![],
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            membership: HashMap::new(),
            authored: None,
            supports_unbounded: false,
        };
        assert_eq!(
            graph.overlaps,
            vec![(0, 1), (1, 2)],
            "the hall touches the corridor and the corridor overlaps room 2"
        );
        assert!(
            reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none(),
            "a reached portal-less corridor cannot bound visibility through unboxed space"
        );
    }

    /// The floor-pose shape of BleakFallsBarrow01 (analysis-1112): the camera's hall 0 has no
    /// portal at all, corridor 1 and room 2 follow through open overlaps, and room 2's only doorway
    /// to the backdrop 3 sits 3,000 units below the view axis - at 4,000 units in front of the
    /// camera the 60-degree view reaches y -2,309, so the aperture projects off screen and the old
    /// walk died here (9 of 34 rooms reached at the real poses). The start rule draws the whole
    /// cell instead, so the backdrop cannot be culled.
    #[test]
    fn a_portal_less_hall_start_draws_the_whole_cell_behind_an_off_screen_portal() {
        let rooms = vec![
            boxed(1, Vec3::ZERO, Vec3::new(1683.0, 928.0, 3488.0)),
            boxed(
                2,
                Vec3::new(-1500.0, 0.0, -3588.0),
                Vec3::new(1000.0, 928.0, 100.0),
            ),
            boxed(
                3,
                Vec3::new(-3300.0, 0.0, -4300.0),
                Vec3::new(900.0, 928.0, 1200.0),
            ),
            boxed(
                4,
                Vec3::new(1500.0, 0.0, -4600.0),
                Vec3::new(800.0, 928.0, 400.0),
            ),
        ];
        let graph = RoomGraph {
            rooms: rooms.clone(),
            portals: vec![doorway(20, Vec3::new(0.0, -3000.0, -4000.0), 150.0, [2, 3])],
            links: vec![],
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            membership: HashMap::new(),
            authored: None,
            supports_unbounded: false,
        };
        assert_eq!(
            graph.overlaps,
            vec![(0, 1), (1, 2)],
            "the hall touches the corridor and the corridor overlaps room 2"
        );
        assert!(
            reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none(),
            "a portal-less start draws the whole cell"
        );
    }

    /// The same shape with a `PortalMarker` between the hall and the corridor: both rooms then
    /// carry a portal, the corridor's overlap edge to the backdrop still narrows (the backdrop has
    /// a portal of its own), and the doorway's small aperture - not the whole screen - is what the
    /// walk carries in. The backdrop's box lies outside that aperture, so it stays culled: the
    /// backdrop behind the wall the rule must keep culling (design-1100 §5).
    #[test]
    fn a_portal_between_the_hall_and_the_corridor_still_culls_the_backdrop() {
        let mut rooms = vec![
            boxed(1, Vec3::ZERO, Vec3::new(1683.0, 928.0, 3488.0)),
            boxed(
                2,
                Vec3::new(-2600.0, 0.0, -4300.0),
                Vec3::new(1200.0, 928.0, 1200.0),
            ),
            boxed(
                3,
                Vec3::new(-1500.0, 0.0, -3900.0),
                Vec3::new(1000.0, 928.0, 400.0),
            ),
        ];
        let mut portals = vec![doorway(20, Vec3::new(0.0, 0.0, -3488.0), 150.0, [0, 1])];
        park_portals(&mut rooms, &mut portals, &[2]);
        assert!(
            overlap_links(&rooms).contains(&(1, 2)),
            "the corridor and the backdrop are one box overlap"
        );
        let graph = RoomGraph {
            // A portal already joins the hall and the corridor, so the pair keeps the doorway's
            // narrowing and gets no implicit edge (as `load_room_graph` builds it).
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: vec![(1, 2)],
            portals,
            rooms,
            ..Default::default()
        };
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(
            &reached[..3],
            [true, true, false],
            "the backdrop is behind the wall"
        );
    }

    /// A pair of rooms that both carry a portal keeps the thick edge: crossing the overlap narrows
    /// the region to the neighbour's box, and the neighbour is culled when its box lies outside the
    /// region the walk arrived with. This is the property the passing poses rest on.
    #[test]
    fn a_pair_of_rooms_that_both_have_portals_still_narrows() {
        let mut rooms = vec![
            small(1, Vec3::ZERO, 6.0),
            small(2, Vec3::new(0.0, 0.0, -18.0), 5.0),
            small(3, Vec3::new(16.0, 0.0, -22.0), 5.0),
        ];
        let mut portals = Vec::new();
        park_portals(&mut rooms, &mut portals, &[0, 1, 2]);
        let graph = RoomGraph {
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            portals,
            rooms,
            ..Default::default()
        };
        assert!(graph.overlaps.contains(&(1, 2)));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(&reached[..3], [true, true, false]);
        // Removing those portals makes the reached corridor incomplete. Even though the camera
        // keeps its own portal, the whole cell must stay drawn.
        let mut open = graph.clone();
        open.portals.clear();
        park_portals(&mut open.rooms, &mut open.portals, &[0]);
        assert!(reached_rooms(&open, Vec3::ZERO, clip_from_local()).is_none());
    }

    /// A touching pair that a doorway joins keeps the doorway's narrowing: no overlap edge is added.
    #[test]
    fn a_pair_joined_by_a_doorway_gets_no_overlap_edge() {
        let connection = database();
        insert_room(&connection, 1, 7, [0.0, 0.0, 0.0]);
        // Touching room 1 along creation X (half_x is 10).
        insert_room(&connection, 2, 7, [20.0, 0.0, 0.0]);
        insert_room(&connection, 3, 7, [40.0, 0.0, 0.0]);
        connection
            .execute(
                "INSERT INTO room_portals VALUES (10,7,10,0,0,0,0,0,4,4,1,2)",
                [],
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        // 1-2 has a doorway, 2-3 touches with none.
        assert_eq!(graph.overlaps, vec![(1, 2)]);
        // Through the doorway a narrow rectangle shows; the pair keeps that narrowing.
        let rooms = vec![
            room(1, Vec3::ZERO, 10.0),
            room(2, Vec3::new(0.0, 0.0, -20.0), 10.0),
        ];
        let mut doorwayed = RoomGraph {
            portals: vec![doorway(10, Vec3::new(0.0, 0.0, -10.0), 1.0, [0, 1])],
            rooms,
            ..Default::default()
        };
        doorwayed.overlaps = vec![];
        let reached = reached_rooms(&doorwayed, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true]);
    }
}
