//! Interior room-and-portal culling for the main camera (upstream issue #188).
//!
//! The converter writes the rooms of an interior cell, the doorways between them and which objects
//! sit in which room (`docs/specs/converters/db-schema.md`, section 15; design
//! `docs/research/interior-room-graph-converter.md`). The tables hold facts; this module is the
//! policy that turns them into "do not draw the rooms the camera cannot see into".
//!
//! # What is drawn
//!
//! Start in every room containing the view’s lookup point in the authored walk, or the eye in
//! the legacy walk, and walk through the portals. At
//! each portal its polygon is clipped against the arriving convex volume. Aperture planes and
//! the plane behind the doorway narrow the next volume. Each room retains the union of its paths,
//! so a gap between visible doorways stays outside the union. Each authored start gets its own FIFO walk in room order.
//! Across all starts, each physical portal carries the room's current union on its first
//! successful crossing through a clear path. Crossings with unresolved aperture occlusion remain
//! eligible for later paths. Authored `XLRM` groups share one master room and the union of their
//! bounds. Legacy links pass volumes unchanged.
//! Cycles and complex graphs have bounded work: exceeding a contribution or plane budget widens
//! that room to the camera frustum for subsequent crossings.
//! Source-verified independent plane and box occluders add exclusions to their owning room or
//! unbounded space. Each path carries its exclusions through later portals; alternative paths
//! remain a union. A mesh is excluded only when its affine AABB's enclosing sphere is fully
//! inside a shadow. Isolated pairs of source-verified planes cast independent shadows.
//! Portal budget widening retains the destination's local occluders. If a reached owner exceeds
//! the remaining view shadow budget, that cell uses portal culling alone. Stateful shapes,
//! complex links, unverified overrides, and ambiguously assigned primitives remain inactive.
//!
//! Versioned graphs distinguish rooms, authored unbounded endpoints, and unknown endpoints. A
//! lookup point outside every room starts in unbounded space when the authored graph is complete.
//! From there, only authored outside portals can lead into rooms. Legacy gaps keep full rendering. Unknown endpoints and incomplete authored graphs draw the whole cell.
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
//!
//! An object with one pivot-containing room is hidden when its mesh bounds miss every candidate
//! volume. The mesh's current AABB is transformed into cell-local space with its full affine
//! transform, including rotation, scale, and shear. Missing or invalid bounds and `NoFrustumCulling`
//! skip the aperture test but retain the unreached-space test. Versioned graphs also track
//! references whose pivots are in no room. Those with overlap rows can use the overlap and
//! unbounded volumes. Those without either kind of row stay drawn in roomed cells.
//! Multiple containing rooms, unknown references, and legacy unassigned
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
//! preserves the incoming visibility volume because room boxes need not bound their meshes.
//! An edge to a portal-less room instead triggers the whole-cell fallback above.
//!
//! The unique authored walk disables these edges and the portal-less-room heuristic. They use the authored
//! master/slave resolution instead. See `docs/research/skyrim-native-culling.md`.
//!
//! # What is never culled by its room
//!
//! The converter records room membership for every placed record, actors included, but a room is
//! where something *was placed*, not where it is. These are treated as in no room and stay drawn:
//!
//! - a reference carrying [`RoomCullExempt`], inserted for actors and doors.
//!   [`RoomCullLegacyExempt`] protects authored overrides, activators and loose inventory items in legacy
//!   graphs; authored assignment preserves `NONE` and updates supported moving reference roots.
//!   **Future actor spawning hooks in here:** whatever spawns an actor
//!   or an animated door outside `spawn_cell` inserts `RoomCullExempt` on the reference entity;
//! - a mesh whose own entity, or whose reference, has a `RigidBody` that moves
//!   (`Dynamic`, `KinematicPositionBased` or `KinematicVelocityBased`), except an authored
//!   reference root, which follows its assignment rule. Moving child meshes still stay drawn.
//!   `Fixed` bodies, which static collision uses, are culled normally.
//! - a mesh with `AnimatedBy` on itself or on an ancestor below its reference root. The mesh can
//!   move independently of the placement used for room assignment. `start_actor_animations` marks
//!   the composed NPC body hierarchy, and those actors already carry `RoomCullExempt`. Bevy also
//!   marks nodes in streamed GLBs with embedded animation targets, including native mod models.
//! - a `SkinnedMesh` with a valid authored room target and no source sphere. Its joints can move
//!   the mesh beyond the named room with the reference root still at its authored placement.
//!
//! Skyrim appears to keep a web's assignment bound fixed while its bones move, so the web can
//! disappear when its room is culled. MudCrab keeps these uncertain meshes in the camera's draw
//! list to avoid hiding geometry that can move independently of the placement. That can add draw
//! work for meshes hidden behind walls. The converted Bleak Falls models `FXcobwebRoundWall01`
//! and `FXspiderWebKitDoorSpecial` have skins without animation clips. In the captured cell their
//! automatic assignments keep them drawn because they lack source spheres. The native bound probes
//! and rendered controls are in `docs/research/skyrim-culling-mismatches.md`.
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
mod occluder_data;
mod occlusion;
mod volume;

#[cfg(test)]
mod native_fixture;
#[cfg(test)]
mod retail_fixture;

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
    gltf::GltfMeshExtras,
    math::Affine3A,
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures_lite::future},
};
use bevy_rapier3d::prelude::RigidBody;
use rusqlite::{Connection, OpenFlags};
use shared::room_assignment::{DRAWABLE_SPHERE_EXTRA, ModelSphere};
use std::{
    any::TypeId,
    collections::{HashMap, HashSet, VecDeque, hash_map::Entry},
    path::PathBuf,
};

// Bound shadow construction across resident interiors and the exclusions carried by any path.
const MAX_VIEW_OCCLUDERS: usize = 64;

/// Marks a placed reference that room culling must never hide, such as an actor or animated door.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RoomCullExempt;

/// Authored policies and loose objects stay exempt until native assignment metadata is available.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RoomCullLegacyExempt;

/// Ownership persists until the reference moves or its cell root is replaced.
#[derive(Default)]
pub struct RoomAssignmentCache(HashMap<Entity, assignment::CachedAssignment>);

/// Keeps the original named attachment until a streamed reference first moves.
#[derive(Component, Clone, Copy)]
pub enum NamedRoomAssignment {
    Initial { root: Entity, placement: Affine3A },
    Moved,
}

pub fn assignment_follows_transform(record_type: Option<&str>) -> bool {
    matches!(
        record_type,
        Some(
            "ACTI"
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

/// Whether a base record type (the `records.record_type` of a reference's base object) is one
/// room culling must not hide: actors and their leveled lists, doors, activators, and the
/// inventory records that become loose, physics-driven objects when placed. Mudcrab keeps
/// these types visible as a precaution. For initial attachment to a supplied multibound node
/// (RVA 0x2d8740) and normal reassignment (RVA 0x2ca230), see docs/research/skyrim-native-culling.md.
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

pub(crate) fn mark_reference_exemptions(
    entity: &mut bevy::ecs::system::EntityCommands,
    record_type: Option<&str>,
    has_override: bool,
) {
    let movable = assignment_follows_transform(record_type);
    if movable || has_override {
        entity.insert(RoomCullLegacyExempt);
    }
    if !movable && exempt_record_type(record_type) {
        entity.insert(RoomCullExempt);
    }
}

/// Opt-in marker for a camera, other than the main one, that culls by room (a portal view).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RoomCullingView;

/// Distance along the camera axis used only to locate its starting rooms.
/// The Skyrim world camera uses the 15 Creation units measured in Treva's Watch.
#[derive(Component, Clone, Copy)]
pub struct RoomCullingNear(f32);

impl RoomCullingNear {
    pub(crate) const SKYRIM: Self = Self(15.0);

    fn world_point(self, view: &GlobalTransform) -> Vec3 {
        view.translation() + view.forward() * self.0
    }
}

/// Skyrim checks the aperture near the portal plane (5 Creation units). Mudcrab keeps an
/// 8-unit band open to avoid hiding the next room when the camera leans into a doorway.
const PORTAL_OPEN_DISTANCE: f32 = 8.0;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    fn from_spaces(mut rooms: Vec<RoomVisibility>) -> Option<Self> {
        let unbounded = rooms.pop()?;
        Some(Self { rooms, unbounded })
    }

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

struct PortalView {
    eye: Vec3,
    clip: Mat4,
    near_plane: Vec4,
}

impl PortalView {
    fn new(eye: Vec3, lookup: Vec3, clip: Mat4) -> Self {
        let plane = clip.row(3) - clip.row(2);
        let mut near_plane = plane / plane.truncate().length();
        if lookup != eye {
            // The player camera uses Skyrim's near distance independently of Bevy's render near.
            near_plane.w = -near_plane.truncate().dot(lookup);
        }
        Self {
            eye,
            clip,
            near_plane,
        }
    }
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

    fn has_valid_aperture(&self) -> bool {
        self.center().is_finite()
            && self.corners.iter().enumerate().all(|(i, &corner)| {
                let next = self.corners[(i + 1) % 4] - corner;
                let previous = self.corners[(i + 3) % 4] - corner;
                next.try_normalize().is_some() && previous.cross(next).try_normalize().is_some()
            })
    }

    fn center(&self) -> Vec3 {
        self.corners.iter().copied().sum::<Vec3>() / 4.0
    }

    // WithinFrustumDistFirst also admits the projection of the eye onto the portal plane.
    // The static trace supports this eye test. Entrance/exit polarity, culling stamp cadence,
    // authored always-open behavior, and handling of rooms with a null bound remain unverified.
    // See docs/research/room-assignment-review-20261117.md, “Retail wall identification”.
    fn admits_projected_eye(&self, view: &PortalView) -> bool {
        let eye = view.eye;
        let view_normal = view.near_plane.truncate();
        let width = self.corners[1] - self.corners[0];
        let height = self.corners[3] - self.corners[0];
        let Some(normal) = height.cross(width).try_normalize() else {
            return false;
        };
        let offset = eye - self.center();
        let distance = normal.dot(offset);
        let toward_plane = -distance * normal;
        let near_distance = view.near_plane.dot(eye.extend(1.0));
        if !(-5.0..5.0).contains(&(distance + near_distance))
            && view_normal.dot(eye + toward_plane) <= view_normal.dot(eye)
        {
            return false;
        }
        let projected = offset + toward_plane;
        let rotation = Mat3::from_cols(width.normalize(), height.normalize(), -normal);
        // WithinFrustumDistFirst (RVA 0xfe94e0) reads Creation rotation rows Y and Z
        // at +0x28 and +0x34. In runtime coordinates those rows select -Z and +Y.
        // A sideways eye can therefore pass the width check at a cardinal doorway.
        rotation.row(2).dot(projected).abs() <= width.length() * 0.5
            && rotation.row(1).dot(projected).abs() <= height.length() * 0.5
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
    pub(crate) occluders: HashMap<PortalSide, Vec<occlusion::Occluder>>,
    /// Full export with authored endpoint provenance and complete reference identities.
    pub supports_unbounded: bool,
    /// The cell lacks positive export completeness or contains invalid room or portal data.
    pub incomplete: bool,
    pub rooms: Vec<Room>,
    pub portals: Vec<Portal>,
    /// `room_links` as undirected edges between room indices.
    pub links: Vec<(usize, usize)>,
    /// Sources of authored links whose targets could not be loaded in this cell.
    pub unresolved_link_rooms: HashSet<usize>,
    /// Inferred connections between touching or overlapping room boxes ([`overlap_links`]).
    /// They pass the incoming volume unchanged; a reached portal-less room draws the whole cell.
    pub overlaps: Vec<(usize, usize)>,
    /// Membership by placed-reference id. The key is one number under three names: the `FormId`
    /// component of a spawned reference (`streaming::spawn_cell` inserts `FormId(reference.form_id)`),
    /// `ReferenceRow::form_id`, which the world database loader fills from `references.id`
    /// (`world::database::REFERENCE_COLUMNS` starts with `r.id`), and `reference_rooms.reference_id`.
    /// It is the placement's id, not the base object's `base_form_id`. A test proves the chain
    /// through the real loader (`the_membership_key_is_the_reference_id_the_loader_delivers`).
    pub membership: HashMap<u32, Membership>,
}

/// Size of Skyrim's room expression before threading appends its three terminals.
/// Geometry may deduplicate equal paths. Every distinct crossed portal still emits records.
#[derive(Clone, Copy, Default)]
struct PortalProgramSize {
    records: u8,
    plane_sets: u8,
    union: bool,
}

impl PortalProgramSize {
    const INHERIT_LIMIT: u8 = 26;

    fn append(&mut self, records: u8, plane_sets: u8) {
        self.records = self.records.saturating_add(records);
        self.plane_sets = self.plane_sets.saturating_add(plane_sets);
    }

    fn arrive(&mut self, incoming: Self, local: Self, owns_occluders: bool) {
        let mut contribution = incoming;
        contribution.append(2, 1); // Portal predicate and its plane-set index.
        contribution.append(local.records, local.plane_sets);
        if incoming.records != 0 || owns_occluders {
            contribution.append(2, 0); // AND group delimiters.
        }
        if self.records != 0 {
            // Retail's outer OR scan also reads operand words. Once index 4 occurs,
            // it counts as a group opener and the next merge wraps the program again.
            if !self.union || self.plane_sets > 4 {
                self.append(2, 0);
            }
            self.union = true;
        }
        self.append(contribution.records, contribution.plane_sets);
    }
}

struct RoomOcclusion {
    volume: VisibilityVolume,
    program: PortalProgramSize,
    owns_markers: bool,
}

struct ViewOcclusion<'a> {
    graph: &'a RoomGraph,
    full: &'a RoomOcclusion,
    view: Option<occlusion::View>,
    remaining: &'a mut usize,
    rooms: HashMap<PortalSide, RoomOcclusion>,
    overflowed: bool,
}

impl ViewOcclusion<'_> {
    fn at(&mut self, room: usize) -> Option<&RoomOcclusion> {
        let side = if room == self.graph.rooms.len() {
            PortalSide::Unbounded
        } else {
            PortalSide::Room(room)
        };
        let (Some(view), Some(candidates)) = (&self.view, self.graph.occluders.get(&side)) else {
            return Some(self.full);
        };
        let local = match self.rooms.entry(side) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                // Charge every candidate, including shapes that fail view checks.
                let Some(remaining) = self.remaining.checked_sub(candidates.len()) else {
                    self.overflowed = true;
                    return None;
                };
                *self.remaining = remaining;
                let mut volume = self.full.volume.clone();
                let mut program = PortalProgramSize::default();
                for occluder in candidates {
                    if let Some(shadow) = occluder.shadow_in(view) {
                        let (records, plane_sets) = occluder.program_size();
                        program.append(records, plane_sets);
                        volume.add_occluder(occluder.ref_id, shadow);
                    }
                }
                entry.insert(RoomOcclusion {
                    volume,
                    program,
                    owns_markers: true,
                })
            }
        };
        Some(local)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PortalVisit {
    Unseen,
    Speculative,
    Crossed,
}

impl RoomGraph {
    fn uses_authored_walk(&self) -> bool {
        // Multi-row markers have no unique endpoint pair. The loader orders them by ref_id.
        self.authored.is_some()
            && !self
                .portals
                .windows(2)
                .any(|pair| pair[0].ref_id == pair[1].ref_id)
    }

    fn add_occluder(&mut self, occluder: occlusion::Occluder) {
        self.occluders
            .entry(occluder.owner)
            .or_default()
            .push(occluder);
    }

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

    /// The rooms reachable through doorways inside the view, including the unbounded node.
    /// `None` means the walk falls back to full rendering.
    /// `clip_from_local` maps cell-local space to clip space.
    ///
    /// Only the unique authored walk starts at `room_point`. Portal and occluder geometry
    /// always use the actual camera position.
    fn visibility_volumes_with_budget(
        &self,
        camera: Vec3,
        room_point: Vec3,
        clip_from_local: Mat4,
        shadow_budget: &mut usize,
    ) -> Option<CellVisibility> {
        let room_count = self.rooms.len();
        if (room_count == 0 && self.occluders.is_empty()) || self.incomplete {
            return None;
        }
        if self.authored.as_ref().is_some_and(|authored| {
            !authored.complete || authored.masters.iter().any(Option::is_none)
        }) {
            return None;
        }
        let outside = room_count;
        // Parallel rays can enter disconnected rooms anywhere across an orthographic near plane.
        if room_count != 0 && clip_from_local.row(3).truncate() == Vec3::ZERO {
            return None;
        }
        let mut doorways = vec![Vec::new(); room_count + 1];
        for (index, portal) in self.portals.iter().enumerate() {
            let [Some(a), Some(b)] = portal.rooms.map(|side| side.index(room_count)) else {
                // Unknown endpoints may conceal outside connectivity. Never interpret them as
                // authored outside, including when the camera starts outside all boxes.
                return None;
            };
            if a == b {
                continue;
            }
            doorways[a].push((index, b));
            doorways[b].push((index, a));
        }
        let authored_walk = self.uses_authored_walk();
        let lookup = if authored_walk { room_point } else { camera };
        let view = PortalView::new(camera, lookup, clip_from_local);
        let mut starts = self.rooms_containing(lookup);
        if starts.is_empty() {
            if room_count > 0 && !(self.supports_unbounded && authored_walk) {
                return None;
            }
            starts.push(outside);
        }
        let full = RoomOcclusion {
            volume: VisibilityVolume::camera(clip_from_local)?,
            program: PortalProgramSize::default(),
            owns_markers: false,
        };
        let mut occlusion = ViewOcclusion {
            graph: self,
            full: &full,
            view: (!self.occluders.is_empty())
                .then(|| occlusion::View::new(camera, clip_from_local))
                .flatten(),
            remaining: shadow_budget,
            rooms: HashMap::new(),
            overflowed: false,
        };
        let walk = |occlusion: &mut ViewOcclusion<'_>| {
            if authored_walk {
                self.walk_authored_portals(&starts, &doorways, occlusion, &view)
            } else {
                self.walk_legacy_portals(&starts, &doorways, occlusion, &view)
            }
        };
        let visibility = walk(&mut occlusion);
        if occlusion.overflowed {
            // Shadows affect authored portal visits and inheritance before mesh tests.
            occlusion.view = None;
            return walk(&mut occlusion);
        }
        visibility
    }

    fn walk_legacy_portals(
        &self,
        starts: &[usize],
        doorways: &[Vec<(usize, usize)>],
        occlusion: &mut ViewOcclusion<'_>,
        view: &PortalView,
    ) -> Option<CellVisibility> {
        let room_count = self.rooms.len();
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
            .iter()
            .map(|&room| (room, occlusion.full.volume.clone()))
            .collect();
        while let Some((room, mut arriving)) = pending.pop() {
            if self.unresolved_link_rooms.contains(&room) {
                return None;
            }
            if room != room_count && !has_portal[room] {
                return None;
            }
            let local = &occlusion.at(room)?.volume;
            arriving.extend_occluders(local);
            let Some(seen) = visibility[room].insert(arriving, local) else {
                continue;
            };
            for &(portal_index, other) in &doorways[room] {
                if let Some((through, _)) =
                    seen.cross_portal(&self.portals[portal_index], view, false)
                {
                    pending.push((other, through));
                }
            }
            for &other in &linked[room] {
                pending.push((other, seen.clone()));
            }
            // Room boxes establish inferred connectivity, not an aperture or a bound on
            // their children's geometry. Preserve the incoming portal volume across them.
            for &other in &overlapping[room] {
                pending.push((other, seen.clone()));
            }
        }
        CellVisibility::from_spaces(visibility)
    }

    fn walk_authored_portals(
        &self,
        starts: &[usize],
        doorways: &[Vec<(usize, usize)>],
        occlusion: &mut ViewOcclusion<'_>,
        view: &PortalView,
    ) -> Option<CellVisibility> {
        let full = &occlusion.full.volume;
        let mut visibility: Vec<_> = (0..doorways.len())
            .map(|_| RoomVisibility::for_authored_portals(self.portals.len()))
            .collect();
        let mut programs = vec![PortalProgramSize::default(); doorways.len()];
        let mut visits = vec![PortalVisit::Unseen; self.portals.len()];
        let mut needs_scan = vec![false; doorways.len()];
        let mut pending = VecDeque::new();
        // Retail drains each start before seeding the next one, sharing portal visits across walks.
        for &start in starts {
            let local = occlusion.at(start)?;
            visibility[start].seed(local.volume.clone());
            programs[start] = local.program;
            if programs[start].records != 0 {
                programs[start].append(2, 0);
            }
            needs_scan[start] = true;
            pending.push_back(start);
            while let Some(room) = pending.pop_front() {
                // Keep FIFO slots: an interleaved arrival can open a missed exit before a
                // competing room crosses it. Skip stale slots once all exits crossed or missed.
                if !std::mem::take(&mut needs_scan[room]) {
                    continue;
                }
                for &(index, other) in &doorways[room] {
                    if visits[index] == PortalVisit::Crossed {
                        continue;
                    }
                    // silent-ok: An invalid or self-linked doorway makes visibility indeterminate.
                    let [source, destination] = visibility.get_disjoint_mut([room, other]).ok()?;
                    let portal = &self.portals[index];
                    let incoming = programs[room];
                    let drop_inherited = other < self.rooms.len()
                        && programs[other].records == 0
                        && incoming.records >= PortalProgramSize::INHERIT_LIMIT;
                    let mut crossings = source
                        .volumes()
                        .iter()
                        .filter_map(|path| path.cross_portal(portal, view, true))
                        .peekable();
                    if crossings.peek().is_none() {
                        continue;
                    }
                    let local = occlusion.at(other)?;
                    let mut changed = false;
                    let first_visit = visits[index] == PortalVisit::Unseen;
                    for (mut through, shadowed) in crossings {
                        // A shadow outside the admitted aperture does not make this crossing speculative.
                        if !shadowed {
                            visits[index] = PortalVisit::Crossed;
                        } else if visits[index] == PortalVisit::Unseen {
                            visits[index] = PortalVisit::Speculative;
                        }
                        if other != start && !drop_inherited {
                            through.extend_occluders(&local.volume);
                            changed |= destination.insert(through, &local.volume).is_some();
                        }
                    }
                    // A speculative exit stays eligible as the destination program evolves.
                    needs_scan[room] |= visits[index] == PortalVisit::Speculative;
                    if other != start {
                        if drop_inherited
                            && let Some((mut through, _)) = full.cross_portal(portal, view, true)
                        {
                            through.extend_occluders(&local.volume);
                            changed |= destination.insert(through, &local.volume).is_some();
                        }
                        if first_visit {
                            let inherited = if drop_inherited {
                                PortalProgramSize::default()
                            } else {
                                incoming
                            };
                            programs[other].arrive(inherited, local.program, local.owns_markers);
                        }
                    }
                    if (visits[index] == PortalVisit::Crossed || changed) && other != start {
                        needs_scan[other] = true;
                        pending.push_back(other);
                    }
                }
            }
        }
        CellVisibility::from_spaces(visibility)
    }

    #[cfg(test)]
    fn visibility_volumes(
        &self,
        camera: Vec3,
        room_point: Vec3,
        clip_from_local: Mat4,
    ) -> Option<CellVisibility> {
        let mut shadow_budget = MAX_VIEW_OCCLUDERS;
        self.visibility_volumes_with_budget(camera, room_point, clip_from_local, &mut shadow_budget)
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
        // A cell-local reference ID proves identity, but empty geometric membership
        // does not establish ownership by the unbounded node of a roomed cell.
        if membership.contained.len() > 1
            || (!self.rooms.is_empty()
                && membership.contained.is_empty()
                && membership.overlapping.is_empty())
        {
            return true;
        }
        // Portal overlap adds endpoint candidates. Keep geometric overlap candidates too:
        // conservative aggregate bounds can touch a portal without establishing exact ownership.
        if let Some(owner_bounds) = reference_bounds.or(bounds) {
            for portal in &self.portals {
                let attached = portal.rooms.iter().any(|side| match side {
                    PortalSide::Room(room) => {
                        membership.contained.contains(room) || membership.overlapping.contains(room)
                    }
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
/// ([`room_tables_present`]); a roomless cell can still carry authored unbounded occluders.
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
    if graph.rooms.is_empty() && !occluder_data::has_occluders(connection, cell_id)? {
        return Ok(graph);
    }
    let completeness_present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='room_graph_cells')",
        [],
        |row| row.get(0),
    )?;
    let complete: Option<i64> = if completeness_present {
        connection.query_row(
            "SELECT (SELECT complete FROM room_graph_cells WHERE cell_id=?1)",
            [cell_id],
            |row| row.get(0),
        )?
    } else {
        None
    };
    graph.incomplete = complete != Some(1)
        || graph.rooms.iter().any(|room| {
            !room.center.is_finite()
                || !room.rotation.is_finite()
                || !room.half.is_finite()
                || !room.half.cmpge(Vec3::ZERO).all()
        });
    let (endpoint_columns, visit_index_column): (bool, bool) = connection.query_row(
        "SELECT count(*) FILTER (WHERE name IN ('unbounded_a','unbounded_b'))=2,
                count(*) FILTER (WHERE name='retail_visit_index')=1
         FROM pragma_table_info('room_portals')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let stats_present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='room_graph_stats')",
        [],
        |row| row.get(0),
    )?;
    graph.supports_unbounded = complete == Some(1) && endpoint_columns && stats_present && connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='visibility_version' AND value=2)", [], |row| row.get::<_,bool>(0))?;
    {
        let provenance = if graph.supports_unbounded {
            "unbounded_a,unbounded_b"
        } else {
            "0,0"
        };
        // A private comparison pack can carry the registration order of one retail load.
        // See docs/research/skyrim-culling-rea-parity.md for the observed portal lists.
        // Accept a complete permutation only. Duplicate physical markers need the legacy walk.
        let captured_order = graph.supports_unbounded
            && visit_index_column
            && connection.query_row(
                "SELECT count(*)>0 AND count(DISTINCT ref_id)=count(*)
               AND count(*) FILTER (WHERE typeof(retail_visit_index)='integer')=count(*)
               AND count(DISTINCT retail_visit_index)=count(*)
               AND min(retail_visit_index)=0 AND max(retail_visit_index)=count(*)-1
             FROM room_portals WHERE cell_id=?1",
                [cell_id],
                |row| row.get::<_, bool>(0),
            )?;
        let order = if captured_order {
            "retail_visit_index"
        } else {
            "ref_id"
        };
        let mut statement = connection.prepare_cached(&format!(
            "SELECT ref_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_z,room_a,room_b,{provenance} FROM room_portals WHERE cell_id=?1 ORDER BY {order}"))?;
        let rows = statement.query_map([cell_id], |row| {
            let side = |index: usize| -> rusqlite::Result<PortalSide> {
                let id = row.get::<_, Option<u32>>(index)?;
                Ok(match id {
                    Some(id) => room_index
                        .get(&id)
                        .copied()
                        .map(PortalSide::Room)
                        .unwrap_or(PortalSide::Unknown),
                    None if row.get_ref(index + 2)? == rusqlite::types::ValueRef::Integer(1) => {
                        PortalSide::Unbounded
                    }
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
                half.iter().all(|value| value.is_finite() && *value > 0.0),
            ))
        })?;
        for portal in rows {
            let (portal, valid) = portal?;
            graph.incomplete |= !valid || !portal.has_valid_aperture();
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
    let assignment_version: Option<i64> = if stats_present {
        connection.query_row(
            "SELECT (SELECT value FROM room_graph_stats WHERE name='assignment_version')",
            [],
            |row| row.get(0),
        )?
    } else {
        None
    };
    if let Some(version) = assignment_version {
        if version == 1 && graph.supports_unbounded {
            graph.authored = assignment::load(
                connection,
                cell_id,
                &graph.rooms,
                &room_index,
                &mut graph.portals,
            )?;
        }
        graph.incomplete |= graph.authored.is_none();
    }
    graph.incomplete |= graph
        .portals
        .iter()
        .any(|portal| portal.rooms.contains(&PortalSide::Unknown));
    if graph.authored.is_some() {
        graph.links.clear();
        graph.unresolved_link_rooms.clear();
    }
    let implicit: Vec<(usize, usize)> = if graph.uses_authored_walk() {
        Vec::new()
    } else {
        let mut pairs = overlap_links(&graph.rooms);
        if let Some(authored) = &graph.authored {
            // The legacy walk still sees the redirected portal endpoints of joined rooms.
            pairs = pairs
                .into_iter()
                .filter_map(|(a, b)| {
                    let (a, b) = (authored.masters[a]?, authored.masters[b]?);
                    (a != b).then_some((a.min(b), a.max(b)))
                })
                .collect();
            pairs.sort_unstable();
            pairs.dedup();
        }
        pairs.into_iter()
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
    for occluder in occluder_data::load(connection, cell_id, &graph, &room_index)? {
        graph.add_occluder(occluder);
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
            (read_drawable_spheres, cull_room_views)
                .chain()
                .in_set(VisibilitySystems::CheckVisibility)
                // The list is only complete once Bevy's own check has filled it.
                .after(check_visibility_cpu_culling),
        );
    }
}

/// A converter-verified bound in a static drawable's vertex coordinates.
#[derive(Component)]
pub struct DrawableRoomSphere(ModelSphere);

type ChangedMeshExtras = (With<Mesh3d>, Changed<GltfMeshExtras>);

fn read_drawable_spheres(
    mut commands: Commands,
    changed: Query<(Entity, &GltfMeshExtras), ChangedMeshExtras>,
    mut removed: RemovedComponents<GltfMeshExtras>,
    existing: Query<(), With<DrawableRoomSphere>>,
) {
    for entity in removed.read() {
        if existing.contains(entity) {
            commands.entity(entity).try_remove::<DrawableRoomSphere>();
        }
    }
    for (entity, extras) in &changed {
        // silent-ok: Invalid extras invalidate any previously cached drawable sphere.
        let sphere = serde_json::from_str::<serde_json::Value>(&extras.value)
            .ok()
            .and_then(|mut value| {
                serde_json::from_value::<ModelSphere>(
                    value.as_object_mut()?.remove(DRAWABLE_SPHERE_EXTRA)?,
                )
                // silent-ok: A malformed sphere invalidates the cached drawable bound.
                .ok()
            })
            .filter(|sphere| sphere.valid() && sphere.radius > 0.0);
        if let Some(sphere) = sphere {
            commands.entity(entity).insert(DrawableRoomSphere(sphere));
        } else if existing.contains(entity) {
            commands.entity(entity).try_remove::<DrawableRoomSphere>();
        }
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
    config: Option<Res<EngineConfig>>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
    roots: Query<&GlobalTransform, InteriorRoot>,
    mut views: Query<
        (
            &GlobalTransform,
            &Projection,
            Option<&RoomCullingNear>,
            Option<&Camera>,
            Has<StreamingCamera>,
            &mut VisibleEntities,
        ),
        Or<(With<StreamingCamera>, With<RoomCullingView>)>,
    >,
    mesh_bounds: Query<
        (
            &Aabb,
            &GlobalTransform,
            Option<&DrawableRoomSphere>,
            Has<bevy::mesh::skinning::SkinnedMesh>,
        ),
        Without<NoFrustumCulling>,
    >,
    reference_bounds: Query<(&ExpectedModelBounds, &GlobalTransform)>,
    placements: Query<(&GlobalTransform, Option<&Transform>)>,
    mut named_targets: ParamSet<(
        Query<
            (Entity, &mut NamedRoomAssignment),
            Or<(
                Added<NamedRoomAssignment>,
                Changed<Transform>,
                Changed<GlobalTransform>,
                Changed<ChildOf>,
            )>,
        >,
        Query<&NamedRoomAssignment>,
    )>,
    parents: Query<&ChildOf>,
    ids: Query<&FormId>,
    exemptions: Query<(
        Option<&RigidBody>,
        Has<RoomCullExempt>,
        Has<RoomCullLegacyExempt>,
        Has<ExpectedModelBounds>,
        Has<bevy::animation::AnimatedBy>,
        Has<bevy::mesh::skinning::SkinnedMesh>,
    )>,
    mut owners: Local<HashMap<Entity, (u32, Entity)>>,
    mut assignments: Local<RoomAssignmentCache>,
) {
    state.culled_last_frame = 0;
    state.main_view_meshes = None;
    // The named attachment is lost on the first move, even off screen, while culling is off,
    // or before the graph loads. Automatic assignment only needs the current placement.
    for (reference, mut target) in &mut named_targets.p0() {
        let NamedRoomAssignment::Initial { root, placement } = *target else {
            continue;
        };
        if let Ok(root_transform) = roots.get(root)
            && reference_placement(reference, root, root_transform, &placements, &parents)
                != Some(placement)
        {
            *target = NamedRoomAssignment::Moved;
            assignments.0.remove(&reference);
        }
    }
    let disabled = config.is_some_and(|config| config.shots_no_room_culling)
        || settings.is_some_and(|settings| !settings.room_culling);
    if disabled {
        state.main_view_meshes = views
            .iter()
            .filter(|(_, _, _, camera, main, _)| {
                *main && camera.is_none_or(|camera| camera.is_active)
            })
            .map(|(_, _, _, _, _, visible)| {
                let listed = visible.get(TypeId::of::<Mesh3d>()).len();
                MainViewMeshes {
                    listed,
                    drawn: listed,
                }
            })
            .last();
        return;
    }
    let mut cells: Vec<u32> = state.graphs.keys().copied().collect();
    cells.sort_unstable();
    let mut culled = 0;
    let mut main_view = None;
    let moved_targets = named_targets.p1();
    if assignments.0.len() > OWNER_CACHE_LIMIT {
        assignments.0.clear();
    }
    for (camera_transform, projection, room_near, camera, is_main, mut visible) in &mut views {
        if camera.is_some_and(|camera| !camera.is_active) {
            continue;
        }
        let listed = visible.get(TypeId::of::<Mesh3d>()).len();
        let clip_from_world =
            projection.get_clip_from_view() * camera_transform.to_matrix().inverse();
        let eye = camera_transform.translation();
        let room_point = room_near.map_or(eye, |near| near.world_point(camera_transform));
        let mut shadow_budget = MAX_VIEW_OCCLUDERS;
        for cell in &cells {
            let loaded = &state.graphs[cell];
            let Ok(root) = roots.get(loaded.root) else {
                continue;
            };
            let world_from_local = root.to_matrix();
            let local_from_world = root.affine().inverse();
            let camera_local = local_from_world.transform_point3(eye);
            let Some(volumes) = loaded.graph.visibility_volumes_with_budget(
                camera_local,
                local_from_world.transform_point3(room_point),
                clip_from_world * world_from_local,
                &mut shadow_budget,
            ) else {
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
                let owns_reference = match &loaded.graph.authored {
                    Some(rules) => rules.has_rule(form_id),
                    None => loaded.graph.membership.contains_key(&form_id),
                };
                if !owns_reference {
                    return true;
                }
                // Re-read every frame: a body can be added after the mesh spawns.
                // Authored rules and source spheres supply ownership independently.
                let exempted =
                    |entity| {
                        exemptions.get(entity).is_ok_and(
                            |(body, marked, legacy, has_model_bounds, animated, skinned)| {
                                marked
                                    || (entity != reference && animated)
                                    || (skinned
                                        && loaded.graph.authored.as_ref().is_some_and(|rules| {
                                            rules.unbounded_named_skin(form_id)
                                        }))
                                    || (entity == reference
                                        && !has_model_bounds
                                        && loaded.graph.authored.is_none())
                                    || (legacy && loaded.graph.authored.is_none())
                                    || (body.is_some_and(|body| body_moves(*body))
                                        && (entity != reference || loaded.graph.authored.is_none()))
                            },
                        )
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
                let bounds = mesh_bounds
                    .get(entity)
                    // silent-ok: A mesh without bounds supplies no geometric evidence for culling.
                    .ok()
                    .and_then(|(aabb, transform, sphere, skinned)| {
                        let local_from_mesh = local_from_world * transform.affine();
                        let mut bounds = MeshBounds::transformed(aabb, local_from_mesh)?;
                        if let Some(sphere) = sphere
                            && loaded.graph.authored.is_some()
                            && !skinned
                        {
                            bounds.set_portal_sphere(sphere.0, local_from_mesh);
                        }
                        Some(bounds)
                    });
                if let Some(authored) = &loaded.graph.authored {
                    let Some(transform) =
                        reference_placement(reference, loaded.root, root, &placements, &parents)
                    else {
                        return true;
                    };
                    let cached = match assignments.0.entry(reference) {
                        Entry::Occupied(entry)
                            if entry.get().root == loaded.root
                                && entry.get().transform == transform =>
                        {
                            entry.into_mut()
                        }
                        entry => {
                            let moved = moved_targets
                                .get(reference)
                                .is_ok_and(|target| matches!(target, NamedRoomAssignment::Moved));
                            let owners = if moved {
                                authored.owners(
                                    form_id,
                                    Some(transform),
                                    &loaded.graph.rooms,
                                    &loaded.graph.portals,
                                )
                            } else {
                                authored.initial_owners(
                                    form_id,
                                    transform,
                                    &loaded.graph.rooms,
                                    &loaded.graph.portals,
                                )
                            };
                            entry
                                .insert_entry(assignment::CachedAssignment {
                                    root: loaded.root,
                                    transform,
                                    owners,
                                })
                                .into_mut()
                        }
                    };
                    return assignment::draws(
                        &cached.owners,
                        &loaded.graph.portals,
                        &volumes,
                        bounds.as_ref(),
                    );
                }
                let owner_bounds = reference_bounds
                    .get(reference)
                    // silent-ok: References without model bounds have no owner bounds to test.
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

fn reference_placement(
    reference: Entity,
    root: Entity,
    root_transform: &GlobalTransform,
    placements: &Query<(&GlobalTransform, Option<&Transform>)>,
    parents: &Query<&ChildOf>,
) -> Option<Affine3A> {
    // silent-ok: Without a reference transform there is no placement for room assignment.
    let (global, local) = placements.get(reference).ok()?;
    // silent-ok: A detached reference has no placement relative to the cell root.
    let parent = parents.get(reference).ok()?.parent();
    // Streamed references are direct children. Their local placement survives rebases.
    if parent == root {
        return Some(local.map_or_else(
            || root_transform.affine().inverse() * global.affine(),
            Transform::compute_affine,
        ));
    }
    parents
        .iter_ancestors(parent)
        .take(31)
        .any(|ancestor| ancestor == root)
        .then(|| root_transform.affine().inverse() * global.affine())
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
mod occluder_subset_tests;

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
        math::{Affine3A, primitives::ViewFrustum},
        mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes},
        transform::TransformPlugin,
    };

    fn reached_rooms(graph: &RoomGraph, camera: Vec3, clip: Mat4) -> Option<Vec<bool>> {
        graph
            .visibility_volumes(camera, camera, clip)
            .map(|visible| {
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

    pub(super) fn authored_database() -> Connection {
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
    fn captured_portal_order_requires_a_complete_cell_permutation() {
        let db = authored_database();
        db.execute_batch(
            "INSERT INTO room_bounds VALUES(1,7,0,0,0,0,0,0,1,1,1,NULL,NULL,0);
             INSERT INTO room_portals VALUES
               (10,7,0,0,0,0,0,0,1,1,1,NULL,0,1),
               (20,7,0,0,0,0,0,0,1,1,1,NULL,0,1),
               (30,8,0,0,0,0,0,0,1,1,1,NULL,0,1);",
        )
        .unwrap();
        let order = || {
            load_room_graph(&db, 7)
                .unwrap()
                .portals
                .into_iter()
                .map(|portal| portal.ref_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(order(), [10, 20]);
        db.execute_batch(
            "ALTER TABLE room_portals ADD COLUMN retail_visit_index INTEGER;
             UPDATE room_portals SET retail_visit_index=1 WHERE ref_id=10;",
        )
        .unwrap();
        for value in ["NULL", "1", "2", "-1", "0.5"] {
            db.execute_batch(&format!(
                "UPDATE room_portals SET retail_visit_index={value} WHERE ref_id=20"
            ))
            .unwrap();
            assert_eq!(order(), [10, 20], "index {value}");
        }
        db.execute_batch("UPDATE room_portals SET retail_visit_index=0 WHERE ref_id=20")
            .unwrap();
        assert_eq!(order(), [20, 10]);
        db.execute_batch("UPDATE room_graph_cells SET complete=0 WHERE cell_id=7")
            .unwrap();
        assert_eq!(order(), [10, 20]);
    }

    pub(super) fn add_occluder_tables(connection: &Connection) {
        connection.execute_batch(
            "INSERT INTO room_graph_stats VALUES('occlusion_version',1),('occlusion_links_verified',1);
             CREATE TABLE room_occluders(ref_id INTEGER PRIMARY KEY,cell_id INTEGER,shape INTEGER,
               pos_x REAL,pos_y REAL,pos_z REAL,rot_x REAL,rot_y REAL,rot_z REAL,
               half_x REAL,half_y REAL,half_z REAL,scale REAL,header_flags INTEGER,
               has_enable_parent INTEGER,has_links INTEGER,target_ref INTEGER);
             CREATE TABLE room_occluder_links(ref_id INTEGER,link_index INTEGER,linked_ref INTEGER);",
        ).unwrap();
    }

    fn occluder_database() -> Connection {
        let connection = authored_database();
        add_occluder_tables(&connection);
        connection
            .execute(
                "INSERT INTO room_occluders VALUES(500,7,3,0,20,0,0,0,0,8,0,8,1,0,0,0,NULL)",
                [],
            )
            .unwrap();
        connection
    }

    #[test]
    fn authored_occluder_filters_roomless_interior_meshes_in_the_main_view() {
        for root in [
            GlobalTransform::IDENTITY,
            GlobalTransform::from(Transform {
                translation: Vec3::new(100.0, 50.0, -30.0),
                rotation: Quat::from_rotation_y(0.4),
                scale: Vec3::new(-2.0, 1.5, 0.75),
            }),
        ] {
            let connection = occluder_database();
            connection.execute_batch(
                "INSERT INTO \"references\"(id,cell_id,base_form_id,pos_y) VALUES(1,7,99,10),(2,7,99,40);
                 INSERT INTO room_reference_rules VALUES(1,7,'auto',NULL),(2,7,'auto',NULL);
                 INSERT INTO room_model_bounds VALUES(99,0,0,0,1);"
            ).unwrap();
            let graph = load_room_graph(&connection, 7).unwrap();
            assert!(graph.rooms.is_empty());
            assert_eq!(graph.occluders.len(), 1);
            assert_eq!(graph.occluders[&PortalSide::Unbounded].len(), 1);
            let mut scene = scene_with(root, root);
            scene
                .app
                .world_mut()
                .resource_mut::<RoomCullingState>()
                .graphs
                .get_mut(&7)
                .unwrap()
                .graph = graph;
            for (mesh, z) in [(scene.near, -10.0), (scene.far, -40.0)] {
                let reference = scene.app.world().get::<ChildOf>(mesh).unwrap().parent();
                scene
                    .app
                    .world_mut()
                    .entity_mut(reference)
                    .insert(root.mul_transform(Transform::from_xyz(0.0, 0.0, z)));
                scene.app.world_mut().entity_mut(mesh).insert((
                    Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
                    root.mul_transform(Transform::from_xyz(0.0, 0.0, z)),
                ));
            }
            run_cull(&mut scene.app);
            let listed = mesh_list(&scene.app, scene.camera);
            assert!(
                listed.contains(&scene.near),
                "foreground mesh remains drawn"
            );
            assert!(!listed.contains(&scene.far), "fully hidden mesh is removed");
            assert!(listed.contains(&scene.loose), "unowned geometry fails open");
            assert!(mesh_list(&scene.app, scene.light).contains(&scene.far));
        }
    }

    #[test]
    fn occluder_scale_keeps_spheres_beyond_the_conservative_edge_visible() {
        let clip = Mat4::perspective_infinite_reverse_rh(std::f32::consts::TAU / 3.0, 1.0, 0.1);
        let visible = |shape, scale, edge| {
            let connection = occluder_database();
            connection
                .execute(
                    "UPDATE room_occluders SET shape=?1,scale=?2,half_x=5,half_y=5,half_z=5",
                    rusqlite::params![shape, scale],
                )
                .unwrap();
            let graph = load_room_graph(&connection, 7).unwrap();
            let view = graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip)
                .unwrap();
            [0.0, edge, 44.0].map(|x| {
                let bounds = MeshBounds::transformed(
                    &Aabb::from_min_max(-Vec3::splat(0.2), Vec3::splat(0.2)),
                    bevy::math::Affine3A::from_translation(Vec3::new(x, 0.0, -40.0)),
                )
                .unwrap();
                view.unbounded.intersects(Some(&bounds))
            })
        };
        let cases = [(3, 2.0, 14.0), (1, 2.0, 14.0), (3, 0.5, 7.0), (1, 0.5, 7.0)];
        assert_eq!(
            cases.map(|(shape, scale, edge)| visible(shape, scale, edge)),
            [[false, true, true]; 4],
            "centerline, edge, and far visibility for {cases:?}"
        );
    }

    #[test]
    fn a_roomless_cell_keeps_meshes_visible_when_its_shadow_budget_overflows() {
        let connection = occluder_database();
        let single = load_room_graph(&connection, 7).unwrap();
        let insert_off_axis_planes = |first_id, last_id| {
            connection
                .execute(
                    "WITH RECURSIVE ids(ref_id) AS (
                       VALUES(?1) UNION ALL SELECT ref_id + 1 FROM ids WHERE ref_id < ?2
                     )
                     INSERT INTO room_occluders
                     SELECT ref_id,7,3,1000,20,0,0,0,0,8,0,8,1,0,0,0,NULL FROM ids",
                    [first_id, last_id],
                )
                .unwrap();
        };
        insert_off_axis_planes(501, 500 + MAX_VIEW_OCCLUDERS - 1);
        let graph = load_room_graph(&connection, 7).unwrap();
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(0.2), Vec3::splat(0.2)),
            bevy::math::Affine3A::from_translation(Vec3::new(0.0, 0.0, -40.0)),
        )
        .unwrap();
        let mut shadow_budget = MAX_VIEW_OCCLUDERS;
        let visible = graph
            .visibility_volumes_with_budget(
                Vec3::ZERO,
                Vec3::ZERO,
                clip_from_local(),
                &mut shadow_budget,
            )
            .unwrap();
        assert!(!visible.unbounded.intersects(Some(&bounds)));
        let visible = single
            .visibility_volumes_with_budget(
                Vec3::ZERO,
                Vec3::ZERO,
                clip_from_local(),
                &mut shadow_budget,
            )
            .unwrap();
        assert!(
            visible.unbounded.intersects(Some(&bounds)),
            "another cell in the same view shares the exhausted budget"
        );

        insert_off_axis_planes(500 + MAX_VIEW_OCCLUDERS, 5499);
        let visible = load_room_graph(&connection, 7)
            .unwrap()
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(visible.unbounded.intersects(Some(&bounds)));
    }

    #[test]
    fn all_null_occluder_links_keep_other_occluders_available() {
        let connection = occluder_database();
        connection
            .execute_batch(
                "INSERT INTO room_occluders VALUES(501,7,3,0,40,0,0,0,0,8,0,8,1,0,0,0,NULL);
             DELETE FROM room_graph_stats WHERE name='occlusion_links_verified';
             UPDATE room_occluders SET cell_id=8,has_links=1 WHERE ref_id=500;",
            )
            .unwrap();
        assert!(
            load_room_graph(&connection, 7)
                .unwrap()
                .occluders
                .is_empty()
        );
        connection
            .execute_batch(
                "INSERT INTO room_graph_stats VALUES ('occlusion_links_verified',1);
                 UPDATE room_occluders SET cell_id=7 WHERE ref_id=500;",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert_eq!(
            graph
                .occluders
                .values()
                .flatten()
                .map(|shape| shape.ref_id)
                .collect::<Vec<_>>(),
            [501]
        );
    }

    #[test]
    fn uncertain_occluder_state_and_legacy_exports_cannot_hide_geometry() {
        for change in [
            "DELETE FROM room_graph_stats WHERE name='occlusion_version'",
            "UPDATE room_graph_cells SET complete=0",
            "UPDATE room_occluders SET header_flags=2048",
            "UPDATE room_occluders SET header_flags=32",
            "UPDATE room_occluders SET header_flags=2147483648",
            "UPDATE room_occluders SET has_enable_parent=1",
            "UPDATE room_occluders SET target_ref=0",
            "UPDATE room_occluders SET target_ref=1",
            "UPDATE room_occluders SET has_links=1",
            "INSERT INTO room_occluder_links VALUES(999,0,500)",
            "UPDATE room_occluders SET scale=0",
            "UPDATE room_occluders SET scale=-1",
            "UPDATE room_occluders SET scale=1e999",
            "UPDATE room_occluders SET scale='invalid'",
            "UPDATE room_occluders SET scale=NULL",
        ] {
            let connection = occluder_database();
            connection.execute_batch(change).unwrap();
            assert!(
                load_room_graph(&connection, 7)
                    .unwrap()
                    .occluders
                    .is_empty(),
                "{change}"
            );
        }
    }

    #[test]
    fn uncertain_plane_pairs_do_not_hide_geometry() {
        let changes = [
            "UPDATE room_occluder_links SET single_target_verified=0",
            "UPDATE room_occluders SET has_enable_parent=1 WHERE ref_id=500",
            "UPDATE room_occluders SET header_flags=2048 WHERE ref_id=501",
            "UPDATE room_occluders SET cell_id=8 WHERE ref_id=500",
            "UPDATE room_occluders SET shape=1,half_y=2 WHERE ref_id=501",
            "DELETE FROM room_occluders WHERE ref_id=500",
            "INSERT INTO room_occluder_links VALUES(800,0,501,1)",
            "ALTER TABLE room_occluder_links DROP COLUMN single_target_verified",
        ];
        for change in changes {
            let db = occluder_database();
            db.execute_batch(
                "ALTER TABLE room_occluder_links ADD COLUMN single_target_verified INTEGER DEFAULT 0;
                 UPDATE room_occluders SET has_links=1;
                 INSERT INTO room_occluders VALUES(501,7,3,8,20,0,0,0,0,8,0,8,1,0,0,1,NULL);
                 INSERT INTO room_occluder_links VALUES(500,1,501,1),(501,3,500,1);"
            ).unwrap();
            assert_eq!(
                load_room_graph(&db, 7).unwrap().occluders[&PortalSide::Unbounded].len(),
                2
            );
            db.execute_batch(change).unwrap();
            assert!(
                load_room_graph(&db, 7).unwrap().occluders.is_empty(),
                "{change}"
            );
        }
    }

    #[test]
    fn explicit_plane_requires_a_verified_target_matching_its_containing_room() {
        let db = occluder_database();
        insert_room(&db, 1, 7, [0.0, 20.0, 0.0]);
        insert_room(&db, 2, 7, [200.0, 20.0, 0.0]);
        db.execute_batch(
            "UPDATE room_bounds SET flags=256; UPDATE room_occluders SET target_ref=1;",
        )
        .unwrap();
        let occluders = || load_room_graph(&db, 7).unwrap().occluders;
        assert!(occluders().is_empty());
        db.execute_batch("INSERT INTO room_reference_rules VALUES(500,7,'target',1)")
            .unwrap();
        assert_eq!(occluders()[&PortalSide::Room(0)].len(), 1);
        db.execute_batch(
            "UPDATE room_reference_rules SET target_ref=2; UPDATE room_occluders SET target_ref=2;",
        )
        .unwrap();
        assert!(occluders().is_empty());
    }

    #[test]
    fn malformed_occluder_values_preserve_portals_and_valid_occluders() {
        use bevy::log::tracing_subscriber::{Layer, Registry, layer::Context, prelude::*};
        use std::{
            fmt::Debug,
            sync::{Arc, Mutex},
        };
        use tracing::{Event, Level, Subscriber, field::Field};

        #[derive(Clone, Default)]
        struct OccluderWarnings(Arc<Mutex<Vec<String>>>);

        impl<S: Subscriber> Layer<S> for OccluderWarnings {
            fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
                let metadata = event.metadata();
                if *metadata.level() == Level::WARN
                    && metadata.target() == "engine::room_culling::occluder_data"
                {
                    event.record(&mut |field: &Field, value: &dyn Debug| {
                        if field.name() == "unusable_rows" {
                            self.0.lock().unwrap().push(format!("{value:?}"));
                        }
                    });
                }
            }
        }

        let connection = occluder_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        connection
            .execute_batch(
                "INSERT INTO room_portals VALUES(10,7,0,0,0,0,0,0,4,6,1,NULL,0,1);
                 INSERT INTO room_occluders VALUES
                    (501,7,3,0,40,0,0,0,0,'invalid',0,8,1,0,0,0,NULL),
                    (502,7,3,0,40,0,0,0,0,8,0,8,'invalid',0,0,0,NULL),
                    (503,7,3,0,40,0,0,0,0,8,0,8,0,0,0,0,NULL),
                    (504,7,3,0,40,0,0,0,0,8,0,8,1,2048,0,0,NULL);",
            )
            .unwrap();
        let warnings = OccluderWarnings::default();
        let subscriber = Registry::default().with(warnings.clone());
        let graph = tracing::subscriber::with_default(subscriber, || {
            load_room_graph(&connection, 7).unwrap()
        });
        assert_eq!(*warnings.0.lock().unwrap(), ["3"]);
        assert_eq!(
            graph.portals[0].rooms,
            [PortalSide::Room(0), PortalSide::Unbounded]
        );
        assert_eq!(
            graph
                .occluders
                .values()
                .flatten()
                .map(|shape| shape.ref_id)
                .collect::<Vec<_>>(),
            [500]
        );

        connection
            .execute_batch("ALTER TABLE room_occluders RENAME COLUMN half_x TO missing_half_x")
            .unwrap();
        assert!(load_room_graph(&connection, 7).is_err());
    }

    #[test]
    fn occluder_ownership_resolves_joined_bounds_and_skips_ambiguous_masters() {
        let connection = occluder_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [100.0, 0.0, 0.0]);
        connection
            .execute_batch(
                "UPDATE room_bounds SET flags=256 WHERE ref_id=1;
             INSERT INTO room_links VALUES(1,2,0),(2,1,0);
             UPDATE room_occluders SET pos_x=100,pos_y=0;",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert_eq!(graph.occluders.len(), 1);
        assert_eq!(graph.occluders[&PortalSide::Room(0)].len(), 1);
        connection
            .execute_batch(
                "INSERT INTO room_reference_rules VALUES(500,7,'target',2);
            UPDATE room_occluders SET target_ref=2;",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert_eq!(graph.occluders.len(), 1);
        assert_eq!(graph.occluders[&PortalSide::Room(0)].len(), 1);
        insert_room(&connection, 3, 7, [100.0, 0.0, 0.0]);
        connection
            .execute_batch("UPDATE room_bounds SET flags=256 WHERE ref_id=3")
            .unwrap();
        assert!(
            load_room_graph(&connection, 7)
                .unwrap()
                .occluders
                .is_empty()
        );
    }

    #[test]
    fn unsupported_assignment_version_draws_the_full_cell() {
        let db = authored_database();
        insert_room(&db, 1, 7, [0.0; 3]);
        db.execute(
            "UPDATE room_graph_stats SET value=2 WHERE name='assignment_version'",
            [],
        )
        .unwrap();
        let graph = load_room_graph(&db, 7).unwrap();
        assert!(graph.incomplete);
        assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
    }

    #[test]
    fn missing_advertised_assignment_table_invalidates_visibility() {
        let db = authored_database();
        insert_room(&db, 1, 7, [0.0; 3]);
        db.execute("DROP TABLE room_model_bounds", []).unwrap();
        assert!(load_room_graph(&db, 7).unwrap().incomplete);
        db.execute(
            "DELETE FROM room_graph_stats WHERE name='assignment_version'",
            [],
        )
        .unwrap();
        assert!(!load_room_graph(&db, 7).unwrap().incomplete);
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
    fn a_named_activator_without_a_sphere_attaches_until_it_moves() {
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
        let root = scene.app.world().resource::<RoomCullingState>().graphs[&7].root;
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .remove::<ExpectedModelBounds>()
            .insert((
                RigidBody::Dynamic,
                Transform::IDENTITY,
                GlobalTransform::IDENTITY,
                NamedRoomAssignment::Initial {
                    root,
                    placement: Affine3A::IDENTITY,
                },
            ));
        mark_reference_exemptions(
            &mut scene.app.world_mut().commands().entity(reference),
            Some("ACTI"),
            true,
        );
        scene.app.world_mut().flush();
        scene.app.world_mut().entity_mut(scene.far).insert((
            Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, -25.0)),
        ));
        scene.app.add_systems(Update, cull_room_views);
        scene.app.world_mut().run_schedule(Update);
        assert!(
            !mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "an explicit initial target supplies ownership without geometric memberships or spheres"
        );
        assert!(mesh_list(&scene.app, scene.camera).contains(&scene.near));
        // A descendant can move independently while the named placement stays in its original room.
        let animated = scene
            .app
            .world_mut()
            .spawn((ChildOf(reference), bevy::animation::AnimatedBy(reference)))
            .id();
        scene
            .app
            .world_mut()
            .entity_mut(scene.far)
            .insert((ChildOf(animated), GlobalTransform::from_xyz(0.0, 0.0, -1.0)));
        scene
            .app
            .world_mut()
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .push(scene.far);
        scene.app.world_mut().run_schedule(Update);
        assert!(mesh_list(&scene.app, scene.camera).contains(&scene.far));
        scene
            .app
            .world_mut()
            .entity_mut(scene.far)
            .insert(ChildOf(reference));
        scene.app.world_mut().run_schedule(Update);
        assert!(!mesh_list(&scene.app, scene.camera).contains(&scene.far));
        scene
            .app
            .insert_resource(crate::settings::RenderingSettings {
                room_culling: false,
                ..Default::default()
            });
        scene.app.world_mut().entity_mut(reference).insert((
            Transform::from_xyz(1.0, 0.0, 0.0),
            GlobalTransform::from_xyz(1.0, 0.0, 0.0),
        ));
        // The far mesh is absent from the list during this move.
        scene.app.world_mut().run_schedule(Update);
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .insert((Transform::IDENTITY, GlobalTransform::IDENTITY));
        scene.app.world_mut().run_schedule(Update);
        scene
            .app
            .world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .room_culling = true;
        scene
            .app
            .world_mut()
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .push(scene.far);
        scene.app.world_mut().run_schedule(Update);
        assert!(
            mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "a moved root without a source sphere stays visible after returning to its original placement"
        );
    }

    #[test]
    fn a_named_skin_without_a_source_sphere_skips_room_culling() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [0.0, 25.0, 0.0]);
        connection
            .execute_batch(
                "INSERT INTO \"references\"(id,cell_id,base_form_id,pos_y) VALUES(101,7,91,25),(102,7,92,25);
                 INSERT INTO room_reference_rules VALUES(101,7,'target',2),(102,7,'target',2);
                 INSERT INTO room_model_bounds VALUES(92,0,0,0,1);",
            )
            .unwrap();
        let mut scene = scene();
        let world = scene.app.world_mut();
        let root = {
            let mut state = world.resource_mut::<RoomCullingState>();
            let loaded = state.graphs.get_mut(&7).unwrap();
            loaded.graph = load_room_graph(&connection, 7).unwrap();
            loaded.root
        };
        let inverse_bindposes = world
            .get_resource_or_init::<Assets<SkinnedMeshInverseBindposes>>()
            .add(SkinnedMeshInverseBindposes::from(vec![Mat4::IDENTITY]));
        let placement = Transform::from_xyz(0.0, 0.0, -25.0);
        let global = GlobalTransform::from(placement);
        let bounds = Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5));
        let [(unbounded, unbounded_skin), (_, bounded_skin)] = [101, 102].map(|form_id| {
            let reference = world
                .spawn((FormId(form_id), ChildOf(root), placement, global))
                .id();
            let joint = world
                .spawn((ChildOf(reference), Transform::IDENTITY, global))
                .id();
            let mesh = world
                .spawn((
                    Mesh3d::default(),
                    ChildOf(reference),
                    bounds,
                    global,
                    SkinnedMesh {
                        inverse_bindposes: inverse_bindposes.clone(),
                        joints: vec![joint],
                    },
                ))
                .id();
            (reference, mesh)
        });
        let unskinned_mesh = world
            .spawn((Mesh3d::default(), ChildOf(unbounded), bounds, global))
            .id();
        *world
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>()) = vec![unbounded_skin, unskinned_mesh, bounded_skin];

        run_cull(&mut scene.app);
        assert_eq!(
            mesh_list(&scene.app, scene.camera),
            [unbounded_skin],
            "the unreached target hides the unskinned sibling and the skin with a source sphere"
        );

        let world = scene.app.world_mut();
        world
            .entity_mut(scene.camera)
            .insert(GlobalTransform::from_xyz(0.0, 0.0, -22.0));
        world
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .extend([unskinned_mesh, bounded_skin]);
        run_cull(&mut scene.app);
        assert_eq!(
            mesh_list(&scene.app, scene.camera),
            [unbounded_skin, unskinned_mesh, bounded_skin],
            "all three meshes draw when the camera enters their room"
        );
    }

    #[test]
    fn offscreen_movement_only_releases_the_named_target_in_its_own_cell() {
        check_offscreen_movement(false);
    }

    #[test]
    fn offscreen_movement_before_graph_load_releases_only_the_moved_target() {
        check_offscreen_movement(true);
    }

    fn check_offscreen_movement(load_after_movement: bool) {
        let db = authored_database();
        db.execute("INSERT INTO room_graph_cells VALUES(8,1)", [])
            .unwrap();
        let mut app = App::new();
        app.insert_resource(RoomCullingState::new(PathBuf::new()))
            .add_systems(Update, cull_room_views);
        let camera = app
            .world_mut()
            .spawn((
                StreamingCamera,
                GlobalTransform::IDENTITY,
                Projection::Perspective(PerspectiveProjection::default()),
                VisibleEntities::default(),
            ))
            .id();
        let [(moved, mesh_a, a), (_, mesh_b, b)] = [(7, 100), (8, 200)].map(|(cell, id)| {
            insert_room(&db, id + 1, cell, [0.0; 3]);
            insert_room(&db, id + 2, cell, [0.0, 25.0, 0.0]);
            db.execute(
                r#"INSERT INTO "references"(id,cell_id,base_form_id) VALUES(?1,?2,99)"#,
                [id, cell],
            )
            .unwrap();
            db.execute(
                "INSERT INTO room_reference_rules VALUES(?1,?2,'target',?3)",
                [id, cell, id + 2],
            )
            .unwrap();
            let graph = load_room_graph(&db, cell).unwrap();
            let world = app.world_mut();
            let root = world
                .spawn((StreamedCellRoot, CellRef(cell), GlobalTransform::IDENTITY))
                .id();
            let reference = world
                .spawn((
                    FormId(id),
                    ChildOf(root),
                    Transform::IDENTITY,
                    GlobalTransform::IDENTITY,
                    NamedRoomAssignment::Initial {
                        root,
                        placement: Affine3A::IDENTITY,
                    },
                ))
                .id();
            (
                reference,
                world.spawn(ChildOf(reference)).id(),
                (cell, root, graph),
            )
        });
        let list_both = |app: &mut App| {
            app.world_mut()
                .get_mut::<VisibleEntities>(camera)
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>())
                .extend([mesh_a, mesh_b]);
        };
        let mut graphs = [a, b].into_iter();
        let mut load_graphs = |app: &mut App| {
            for (cell, root, graph) in graphs.by_ref() {
                app.world_mut()
                    .resource_mut::<RoomCullingState>()
                    .insert_graph(cell, root, graph);
            }
        };
        if !load_after_movement {
            load_graphs(&mut app);
            list_both(&mut app);
        }
        app.world_mut().run_schedule(Update);
        assert!(mesh_list(&app, camera).is_empty());

        // Both meshes are off screen while only the first reference moves and returns.
        for transform in [Transform::from_xyz(1.0, 0.0, 0.0), Transform::IDENTITY] {
            app.world_mut()
                .entity_mut(moved)
                .insert((transform, GlobalTransform::from(transform)));
            app.world_mut().run_schedule(Update);
        }
        if load_after_movement {
            load_graphs(&mut app);
        }
        list_both(&mut app);
        app.world_mut().run_schedule(Update);
        assert_eq!(mesh_list(&app, camera), [mesh_a]);
    }

    #[test]
    fn tiny_scale_changes_reassign_a_cached_offset_source_sphere() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [10003.0, 0.0, 0.0]);
        insert_room(&connection, 2, 7, [9998.0, 0.0, 0.0]);
        connection
            .execute_batch(
                "UPDATE room_bounds SET half_x=2;
             INSERT INTO \"references\"(id,cell_id,base_form_id) VALUES(2,7,99);
             INSERT INTO room_reference_rules VALUES(2,7,'auto',NULL);
             INSERT INTO room_model_bounds VALUES(99,9998,0,-5,0.1);",
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
            .insert((Transform::IDENTITY, GlobalTransform::IDENTITY));
        scene
            .app
            .world_mut()
            .entity_mut(scene.camera)
            .insert(GlobalTransform::from_translation(Vec3::new(
                10003.0, 0.0, 0.0,
            )));
        scene.app.world_mut().entity_mut(scene.far).insert((
            Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
            GlobalTransform::from_translation(Vec3::new(9998.0, 0.0, -5.0)),
        ));
        // A stored system preserves its Local assignment cache between frames.
        scene.app.add_systems(Update, cull_room_views);
        scene.app.world_mut().run_schedule(Update);
        assert!(!mesh_list(&scene.app, scene.camera).contains(&scene.far));
        let moved = Transform::from_scale(Vec3::splat(1.0005));
        let center = moved
            .to_matrix()
            .transform_point3(Vec3::new(9998.0, 0.0, -5.0));
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .insert((moved, GlobalTransform::from(moved)));
        scene
            .app
            .world_mut()
            .entity_mut(scene.far)
            .insert(GlobalTransform::from_translation(center));
        scene
            .app
            .world_mut()
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .push(scene.far);
        scene.app.world_mut().run_schedule(Update);
        assert!(
            mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "a sub-tolerance scale change moves the offset sphere into the visible room"
        );
    }

    #[test]
    fn named_targets_survive_transformed_cell_roots_and_rebasing() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [0.0, -25.0, 0.0]);
        connection.execute_batch(
            "INSERT INTO \"references\"(id,cell_id,base_form_id,pos_x,pos_y,pos_z) VALUES(2,7,99,0.3,0.5,0.7);
             INSERT INTO room_reference_rules VALUES(2,7,'target',1);
             INSERT INTO room_model_bounds VALUES(99,0,0,25,0.1);"
        ).unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        let mut scene = scene();
        let root = {
            let mut state = scene.app.world_mut().resource_mut::<RoomCullingState>();
            let loaded = state.graphs.get_mut(&7).unwrap();
            loaded.graph = graph;
            loaded.root
        };
        let reference = scene
            .app
            .world()
            .get::<ChildOf>(scene.far)
            .unwrap()
            .parent();
        let placement = Transform::from_xyz(0.3, 0.7, -0.5);
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .insert(NamedRoomAssignment::Initial {
                root,
                placement: placement.compute_affine(),
            });
        let translated = GlobalTransform::from_xyz(8192.125, -12345.75, 32768.5);
        assert_ne!(
            translated.affine().inverse() * translated.mul_transform(placement).affine(),
            placement.compute_affine(),
            "the unchanged placement must exercise world-composition rounding"
        );
        scene.app.add_systems(Update, cull_room_views);
        for root_transform in [
            translated,
            GlobalTransform::IDENTITY,
            GlobalTransform::from(
                Transform::from_xyz(-4000.25, 100.0, 2000.125)
                    .with_rotation(Quat::from_rotation_y(0.37))
                    .with_scale(Vec3::splat(1.25)),
            ),
        ] {
            scene
                .app
                .world_mut()
                .entity_mut(root)
                .insert(root_transform);
            scene
                .app
                .world_mut()
                .entity_mut(reference)
                .insert((placement, root_transform.mul_transform(placement)));
            scene
                .app
                .world_mut()
                .entity_mut(scene.camera)
                .insert(root_transform);
            scene.app.world_mut().entity_mut(scene.far).insert((
                Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
                root_transform.mul_transform(Transform::from_xyz(0.0, 0.0, -5.0)),
            ));
            *scene
                .app
                .world_mut()
                .get_mut::<VisibleEntities>(scene.camera)
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>()) = vec![scene.far];
            scene.app.world_mut().run_schedule(Update);
            assert!(
                mesh_list(&scene.app, scene.camera).contains(&scene.far),
                "an unchanged named target must survive the initial root and later rebases"
            );
        }
    }

    #[test]
    fn unlisted_named_reference_rechecks_ownership_after_returning_to_spawn() {
        AsyncComputeTaskPool::get_or_init(Default::default);
        let db = authored_database();
        insert_room(&db, 1, 7, [0.0; 3]);
        insert_room(&db, 2, 7, [100.0, 0.0, 0.0]);
        db.execute_batch(
            "INSERT INTO \"references\"(id,cell_id,base_form_id) VALUES(2,7,99);
            INSERT INTO room_reference_rules VALUES(2,7,'target',2);
            INSERT INTO room_model_bounds VALUES(99,0,0,-5,0.1);",
        )
        .unwrap();
        let graph = load_room_graph(&db, 7).unwrap();
        let mut scene = scene();
        let root = scene.app.world().resource::<RoomCullingState>().graphs[&7].root;
        scene
            .app
            .world_mut()
            .resource_mut::<RoomCullingState>()
            .graphs
            .get_mut(&7)
            .unwrap()
            .graph = graph.clone();
        let reference = scene
            .app
            .world()
            .get::<ChildOf>(scene.far)
            .unwrap()
            .parent();
        scene.app.world_mut().entity_mut(reference).insert((
            Transform::IDENTITY,
            GlobalTransform::IDENTITY,
            NamedRoomAssignment::Initial {
                root,
                placement: Affine3A::IDENTITY,
            },
        ));
        scene.app.add_systems(Update, cull_room_views);
        scene.app.world_mut().run_schedule(Update);
        assert!(
            !mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "initial named attachment owns the mesh"
        );
        // The mesh is unlisted during both changes, with culling disabled and no graph loaded.
        scene
            .app
            .insert_resource(crate::settings::RenderingSettings {
                room_culling: false,
                ..default()
            });
        scene
            .app
            .world_mut()
            .resource_mut::<RoomCullingState>()
            .graphs
            .remove(&7);
        for placement in [Transform::from_xyz(40.0, 0.0, 0.0), Transform::IDENTITY] {
            scene
                .app
                .world_mut()
                .entity_mut(reference)
                .insert((placement, GlobalTransform::from(placement)));
            scene.app.world_mut().run_schedule(Update);
        }
        scene
            .app
            .insert_resource(crate::settings::RenderingSettings::default());
        scene
            .app
            .world_mut()
            .resource_mut::<RoomCullingState>()
            .insert_graph(7, root, graph);
        scene
            .app
            .world_mut()
            .get_mut::<VisibleEntities>(scene.camera)
            .unwrap()
            .get_mut(TypeId::of::<Mesh3d>())
            .push(scene.far);
        scene.app.world_mut().run_schedule(Update);
        assert!(
            mesh_list(&scene.app, scene.camera).contains(&scene.far),
            "movement returns the mesh to its visible geometric owner"
        );
    }

    #[test]
    fn named_targets_recheck_scale_changes_before_the_first_cull() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [10003.0, 0.0, 0.0]);
        insert_room(&connection, 2, 7, [9998.0, 0.0, 0.0]);
        connection
            .execute_batch(
                "UPDATE room_bounds SET half_x=2;
             INSERT INTO \"references\"(id,cell_id,base_form_id) VALUES(2,7,99);
             INSERT INTO room_reference_rules VALUES(2,7,'target',2);
             INSERT INTO room_model_bounds VALUES(99,9998,0,-5,0.1);",
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
            .insert((Transform::IDENTITY, GlobalTransform::IDENTITY));
        scene
            .app
            .world_mut()
            .entity_mut(scene.camera)
            .insert(GlobalTransform::from_translation(Vec3::new(
                10003.0, 0.0, 0.0,
            )));
        scene.app.world_mut().entity_mut(scene.far).insert((
            Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
            GlobalTransform::from_translation(Vec3::new(9998.0, 0.0, -5.0)),
        ));
        let moved = Transform::from_scale(Vec3::splat(1.0005));
        let center = moved
            .to_matrix()
            .transform_point3(Vec3::new(9998.0, 0.0, -5.0));
        scene
            .app
            .world_mut()
            .entity_mut(reference)
            .insert((moved, GlobalTransform::from(moved)));
        scene
            .app
            .world_mut()
            .entity_mut(scene.far)
            .insert(GlobalTransform::from_translation(center));
        // The placement changes before the Local cache has ever seen this reference.
        scene.app.add_systems(Update, cull_room_views);
        for _ in 0..2 {
            scene.app.world_mut().run_schedule(Update);
            assert!(
                mesh_list(&scene.app, scene.camera).contains(&scene.far),
                "a changed initial transform must not cache the old named room"
            );
        }
    }

    #[test]
    fn current_source_bound_assignment_replaces_stale_membership_for_a_moving_reference() {
        assert!(assignment_follows_transform(Some("ACTI")));
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

    pub(super) fn insert_room(connection: &Connection, id: u32, cell: u32, position: [f32; 3]) {
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
        assert!(!graph.incomplete);
        assert_eq!(
            graph.portals[0].rooms,
            [PortalSide::Room(0), PortalSide::Unbounded]
        );
        assert_eq!(graph.membership[&100], Membership::default());
        assert!(!graph.membership.contains_key(&101));
        assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_some());
        for flag in [
            rusqlite::types::Value::Integer(0),
            rusqlite::types::Value::Integer(2),
            "invalid".to_owned().into(),
        ] {
            connection
                .execute("UPDATE room_portals SET unbounded_b=?1", [flag])
                .unwrap();
            let graph = load_room_graph(&connection, 7).unwrap();
            assert_eq!(graph.portals[0].rooms[1], PortalSide::Unknown);
            assert!(graph.incomplete);
            assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
        }
        // An inconsistent foreign-room ID must not acquire outside meaning from a flag.
        insert_room(&connection, 999, 8, [0.0; 3]);
        connection
            .execute("UPDATE room_portals SET room_b=999,unbounded_b=1", [])
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        assert_eq!(graph.portals[0].rooms[1], PortalSide::Unknown);
        assert!(graph.incomplete);
        assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
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
    fn portal_clipping_requires_a_complete_cell_and_a_positive_aperture() {
        let connection = database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [0.0, 50.0, 0.0]);
        connection
            .execute_batch(
                "INSERT INTO room_portals VALUES (10,7,0,25,0,0,0,0,1,2,1,2);
                 INSERT INTO reference_rooms VALUES (500,2,1);",
            )
            .unwrap();
        // Inside the second room and camera frustum, beside the only exported aperture.
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
            Affine3A::from_translation(Vec3::new(8.0, 0.0, -50.0)),
        )
        .unwrap();
        let draws_mesh = || {
            let graph = load_room_graph(&connection, 7).unwrap();
            graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .is_none_or(|visible| graph.mesh_is_drawn(500, &visible, Some(&bounds), None))
        };
        assert!(draws_mesh(), "legacy pack without a completeness table");
        connection.execute_batch(
            "CREATE TABLE room_graph_cells (cell_id INTEGER PRIMARY KEY, complete INTEGER NOT NULL);
             INSERT INTO room_graph_cells VALUES (8,1);",
        ).unwrap();
        assert!(draws_mesh(), "only the neighboring cell is verified");
        connection
            .execute("INSERT INTO room_graph_cells VALUES (7,2)", [])
            .unwrap();
        assert!(
            draws_mesh(),
            "an invalid completeness flag cannot enable clipping"
        );
        connection
            .execute("UPDATE room_graph_cells SET complete=1 WHERE cell_id=7", [])
            .unwrap();
        assert!(!draws_mesh(), "the verified cell clips to its aperture");
        for half in [(0.0, 2.0), (1.0, 0.0)] {
            connection
                .execute("UPDATE room_portals SET half_x=?1,half_z=?2", half)
                .unwrap();
            assert!(
                draws_mesh(),
                "a certified old pack with extent {half:?} must draw the mesh"
            );
        }
        let falls_back = [
            (1.0, 2.0),
            (1.0e8, 1.0e8),
            (1.0e10, 1.0e10),
            (1.0e-10, 2.0),
            (1.0e20, 1.0e20),
        ]
        .map(|half| {
            connection
                .execute(
                    "UPDATE room_portals SET pos_x=1,pos_z=12,half_x=?1,half_z=?2",
                    half,
                )
                .unwrap();
            let graph = load_room_graph(&connection, 7).unwrap();
            let portal = &graph.portals[0];
            assert!(portal.corners.iter().all(|point| point.is_finite()));
            graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .is_none()
        });
        assert_eq!(
            falls_back,
            [false, false, true, true, true],
            "collapsed apertures and non-normalizable edge planes must draw the whole cell"
        );
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
            occluders: HashMap::new(),
        }
    }

    #[test]
    fn a_reached_room_with_a_dropped_link_draws_the_whole_cell() {
        let connection = database();
        connection
            .execute_batch(
                "CREATE TABLE room_graph_cells (cell_id INTEGER PRIMARY KEY, complete INTEGER NOT NULL);
                 INSERT INTO room_graph_cells VALUES (7,1);",
            )
            .unwrap();
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
    fn an_orthographic_near_plane_cannot_seed_room_traversal_at_one_point() {
        let graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        let clip = Mat4::orthographic_rh(-30.0, 30.0, -30.0, 30.0, 100.0, 0.1);
        assert!(reached_rooms(&graph, Vec3::ZERO, clip).is_none());
    }

    #[test]
    fn a_doorway_in_view_reaches_the_room_behind_it() {
        let graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true]);
    }

    #[test]
    fn duplicate_authored_portals_keep_a_portal_less_start_open() {
        let db = authored_database();
        for id in 1..=3 {
            insert_room(&db, id, 7, [0.0, 30.0 * (id - 1) as f32, 0.0]);
        }
        let mut graph = load_room_graph(&db, 7).unwrap();
        let gate = doorway(10, Vec3::new(0.0, 0.0, -45.0), 2.0, [1, 2]);
        graph.portals.push(gate.clone());
        assert!(!reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap()[1]);
        graph.portals.push(gate);
        assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
    }

    #[test]
    fn duplicate_marker_fallback_restores_overlap_visibility() {
        let db = authored_database();
        insert_room(&db, 1, 7, [0.0; 3]);
        insert_room(&db, 2, 7, [18.0, 0.0, 0.0]);
        insert_room(&db, 3, 7, [100.0, 0.0, 0.0]);
        db.execute_batch(
            "INSERT INTO room_portals VALUES
             (10,7,1000,-200,0,0,0,0,4,6,1,3,0,0),
             (20,7,1000,-200,0,0,0,0,4,6,2,3,0,0);",
        )
        .unwrap();
        let reach = || {
            reached_rooms(
                &load_room_graph(&db, 7).unwrap(),
                Vec3::ZERO,
                clip_from_local(),
            )
            .unwrap()
        };
        assert_eq!(reach(), [true, false, false]);

        // A second endpoint pair belongs to the same physical marker.
        db.execute_batch(
            "ALTER TABLE room_portals RENAME TO single_pairs;
             CREATE VIEW room_portals AS SELECT * FROM single_pairs UNION ALL
             SELECT 10,7,1000,-200,0,0,0,0,4,6,2,3,0,0;",
        )
        .unwrap();
        assert_eq!(reach(), [true, true, false]);

        // Put the touching box on a joined child. Its master still owns the doorway.
        insert_room(&db, 4, 7, [0.0; 3]);
        db.execute_batch(
            "UPDATE room_bounds SET pos_x=-100,flags=256 WHERE ref_id=1;
             INSERT INTO room_links VALUES(1,4,0),(4,1,0);",
        )
        .unwrap();
        assert_eq!(reach(), [true, true, false, false]);
    }

    #[test]
    fn close_portal_only_counts_shadows_in_the_camera_aperture() {
        let clip = Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.1);
        let view = PortalView::new(Vec3::ZERO, Vec3::ZERO, clip);
        let portal = doorway(10, Vec3::new(0.0, 0.0, -7.0), 10.0, [0, 1]);
        for (x, expected) in [(8.0, false), (2.0, true)] {
            let mut volume = VisibilityVolume::camera(clip).unwrap();
            let plane = occlusion::Occluder::from_creation(
                1,
                3,
                [x, 6.0, 0.0],
                [0.0; 3],
                [0.25, 0.0, 0.25],
                1.0,
                PortalSide::Room(0),
            )
            .unwrap();
            volume.add_occluder(
                1,
                plane
                    .shadow_in(&occlusion::View::new(Vec3::ZERO, clip).unwrap())
                    .unwrap(),
            );
            let (_, shadowed) = volume.cross_portal(&portal, &view, true).unwrap();
            assert_eq!(shadowed, expected);
        }
    }

    #[test]
    fn a_close_second_portal_must_intersect_the_inherited_aperture() {
        let db = authored_database();
        insert_room(&db, 1, 7, [0.0; 3]);
        insert_room(&db, 2, 7, [0.0, 30.0, 0.0]);
        insert_room(&db, 3, 7, [0.0, 60.0, 0.0]);
        let mut graph = load_room_graph(&db, 7).unwrap();
        assert!(graph.authored.is_some());
        let entrance = doorway(10, Vec3::new(0.0, 0.0, -20.0), 1.0, [0, 1]);
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(0.2), Vec3::splat(0.2)),
            Affine3A::from_translation(Vec3::new(0.0, 0.0, -40.0)),
        )
        .unwrap();
        let side_exit = |x| {
            let mut exit = side_doorway(11, Vec3::new(x, 0.0, -14.0), 11.0, [1, 2]);
            for corner in &mut exit.corners {
                corner.y /= 11.0;
            }
            exit
        };
        for (exit, reaches_far_room) in [
            (doorway(11, Vec3::new(3.0, 0.0, -3.0), 0.5, [1, 2]), false),
            (doorway(11, Vec3::new(0.0, 0.0, -3.0), 0.5, [1, 2]), false),
            (side_exit(3.0), false),
            (side_exit(0.5), true),
        ] {
            assert!(exit.distance_to(Vec3::ZERO) < PORTAL_OPEN_DISTANCE);
            let exit_center = exit.center();
            graph.portals = vec![entrance.clone(), exit];
            let visible = graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .unwrap();
            assert!(visible.rooms[1].is_reached());
            assert_eq!(
                visible.rooms[2].is_reached(),
                reaches_far_room,
                "{exit_center}"
            );
            assert_eq!(visible.rooms[2].intersects(Some(&bounds)), reaches_far_room);
        }

        // A close entrance can still open the route when it sits outside the camera frustum.
        let clip = Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_3, 1.0, 8.0);
        graph.portals = vec![
            doorway(10, Vec3::new(3.0, 0.0, -3.0), 0.5, [0, 1]),
            doorway(11, Vec3::new(0.0, 0.0, -30.0), 1.0, [1, 2]),
        ];
        assert!(reached_rooms(&graph, Vec3::ZERO, clip).unwrap()[2]);
    }

    #[test]
    fn consecutive_in_band_portals_keep_the_camera_volume() {
        let db = authored_database();
        insert_room(&db, 1, 7, [0.0; 3]);
        insert_room(&db, 2, 7, [0.0, 30.0, 0.0]);
        insert_room(&db, 3, 7, [0.0, 60.0, 0.0]);
        let mut graph = load_room_graph(&db, 7).unwrap();
        graph.portals = vec![
            doorway(10, Vec3::new(3.0, 0.0, -3.0), 0.5, [0, 1]),
            doorway(11, Vec3::new(-3.0, 0.0, -6.0), 0.5, [1, 2]),
        ];
        assert!(
            graph
                .portals
                .iter()
                .all(|p| p.distance_to(Vec3::ZERO) < PORTAL_OPEN_DISTANCE)
        );
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        for (x, drawn) in [(0.0, true), (30.0, false)] {
            let bounds = MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(0.2), Vec3::splat(0.2)),
                Affine3A::from_translation(Vec3::new(x, 0.0, -40.0)),
            )
            .unwrap();
            assert_eq!(
                visible.rooms[2].intersects(Some(&bounds)),
                drawn,
                "mesh at x={x}"
            );
        }
    }

    #[test]
    fn occluder_exclusions_follow_their_portal_path_into_later_rooms() {
        let mut graph = chain(doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]));
        graph.add_occluder(
            occlusion::Occluder::from_creation(
                500,
                3,
                [0.0, 20.0, 0.0],
                [0.0; 3],
                [2.0, 0.0, 2.0],
                1.0,
                PortalSide::Room(1),
            )
            .unwrap(),
        );
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(0.2), Vec3::splat(0.2)),
            bevy::math::Affine3A::from_translation(Vec3::new(0.0, 0.0, -40.0)),
        )
        .unwrap();
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(visible.rooms[2].is_reached());
        assert!(
            !visible.rooms[2].intersects(Some(&bounds)),
            "inherited exclusion hides the far mesh"
        );
        // An alternate direct aperture does not inherit the middle room's occluder.
        graph
            .portals
            .push(doorway(12, Vec3::new(0.0, 0.0, -10.0), 3.0, [0, 2]));
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(visible.rooms[2].intersects(Some(&bounds)));
    }

    #[test]
    fn only_reached_owners_spend_the_shadow_budget() {
        let mut graph = chain(side_doorway(11, Vec3::new(10.0, 0.0, -12.0), 1.5, [1, 2]));
        let add_plane = |graph: &mut RoomGraph, id, owner, depth| {
            graph.add_occluder(
                occlusion::Occluder::from_creation(
                    id,
                    3,
                    [0.0, depth, 0.0],
                    [0.0; 3],
                    [2.0, 0.0, 2.0],
                    1.0,
                    PortalSide::Room(owner),
                )
                .unwrap(),
            );
        };
        add_plane(&mut graph, 500, 1, 20.0);
        for id in 0..MAX_VIEW_OCCLUDERS as u32 {
            add_plane(&mut graph, 1000 + id, 2, 40.0);
        }
        let bounds = |x| {
            MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(0.2), Vec3::splat(0.2)),
                bevy::math::Affine3A::from_translation(Vec3::new(x, 0.0, -25.0)),
            )
            .unwrap()
        };
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(!visible.rooms[2].is_reached());
        assert!(!visible.rooms[1].intersects(Some(&bounds(0.0))));

        graph.portals[1] = doorway(11, Vec3::new(0.0, 0.0, -30.0), 3.0, [1, 2]);
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(visible.rooms[2].is_reached());
        assert!(
            visible.rooms[1].intersects(Some(&bounds(0.0))),
            "overflow also drops the shadows already added in an earlier room"
        );
        assert!(
            !visible.rooms[1].intersects(Some(&bounds(9.0))),
            "the doorway still narrows the portal volume"
        );
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
    fn authored_unbounded_start_keeps_the_projected_portal_and_tests_mesh_bounds() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [-100.0, 120.0, 0.0]);
        insert_room(&connection, 2, 7, [100.0, -120.0, 0.0]);
        connection
            .execute(
                "INSERT INTO room_portals(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,
                half_x,half_z,room_a,room_b,unbounded_a,unbounded_b)
             VALUES(10,7,-100,110,0,0,0,0,4,4,1,NULL,0,1)",
                [],
            )
            .unwrap();
        let mut graph = load_room_graph(&connection, 7).unwrap();
        let projection = Mat4::perspective_infinite_reverse_rh(60_f32.to_radians(), 1.0, 0.1);
        for (toward_portal, reached) in [(-0.02, true), (0.02, false)] {
            let forward = Vec3::new(-1.0, 0.0, toward_portal).normalize();
            let clip = projection * Mat4::look_to_rh(Vec3::ZERO, forward, Vec3::Y);
            let visible = graph
                .visibility_volumes(Vec3::ZERO, forward * 15.0, clip)
                .unwrap();
            assert!(visible.unbounded.is_reached());
            assert_eq!(visible.rooms[0].is_reached(), reached);
            assert!(!visible.rooms[1].is_reached());
            // The first probe is beyond the visible screen. The second is on screen,
            // but outside the doorway cone. Neither should draw in the destination room.
            for center in [Vec3::new(-110.0, 0.0, -121.0), forward * 150.0] {
                let bounds = Aabb::from_min_max(center - Vec3::ONE, center + Vec3::ONE);
                let mesh = MeshBounds::transformed(&bounds, Affine3A::IDENTITY).unwrap();
                assert!(!visible.rooms[0].intersects(Some(&mesh)));
            }
        }
        // A repeated physical marker has no unique pair of endpoints.
        graph.portals.push(graph.portals[0].clone());
        assert!(
            graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, projection)
                .is_none()
        );
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
            let visible = graph.visibility_volumes(
                camera,
                camera,
                clip_from_local() * Mat4::from_translation(-camera),
            );
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
    fn invalid_extents_override_exported_completeness_only_in_the_affected_cell() {
        for invalid_room in [true, false] {
            let connection = database();
            connection.execute_batch(
                "CREATE TABLE room_graph_cells (cell_id INTEGER PRIMARY KEY, complete INTEGER NOT NULL);
                 INSERT INTO room_graph_cells VALUES (7,1),(8,1);",
            ).unwrap();
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
                "invalid_room={invalid_room}: invalid geometry overrides the completeness row"
            );
            assert!(
                graph
                    .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                    .is_none()
            );
            assert!(!load_room_graph(&connection, 8).unwrap().incomplete);
        }
    }

    #[test]
    fn empty_or_collapsed_portals_fall_back_to_full_cell_visibility() {
        let db = authored_database();
        insert_room(&db, 1, 7, [0.0; 3]);
        db.execute_batch("INSERT INTO room_portals VALUES(10,7,0,20,0,0,0,0,4,4,1,NULL,0,1)")
            .unwrap();
        for (x, width, height) in [
            (0.0, 0.0, 0.0),
            (0.0, 4.0, 0.0),
            (1.0e20, 4.0, 4.0),
            (0.0, f32::INFINITY, 4.0),
        ] {
            db.execute(
                "UPDATE room_portals SET pos_x=?1,half_x=?2,half_z=?3",
                rusqlite::params![x, width, height],
            )
            .unwrap();
            let graph = load_room_graph(&db, 7).unwrap();
            assert!(graph.incomplete);
            assert!(reached_rooms(&graph, Vec3::ZERO, clip_from_local()).is_none());
        }
    }

    #[test]
    fn unbounded_camera_positions_without_coverage_keep_full_rendering() {
        let mut graph = outside_graph();
        assert!(
            graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .is_none()
        );
        graph.add_occluder(
            occlusion::Occluder::from_creation(
                500,
                3,
                [0.0, 20.0, 0.0],
                [0.0; 3],
                [8.0, 0.0, 8.0],
                1.0,
                PortalSide::Unbounded,
            )
            .unwrap(),
        );
        assert!(
            graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .is_none(),
            "occluders do not prove portal coverage when the camera is outside room bounds"
        );
    }

    #[test]
    fn unassigned_geometry_stays_visible_beside_a_clipped_outside_aperture() {
        let graph = outside_graph();
        let eye = Vec3::new(0.0, 0.0, -20.0);
        let clip = clip_from_local()
            * Mat4::from_rotation_y(std::f32::consts::PI)
            * Mat4::from_translation(-eye);
        let visible = graph.visibility_volumes(eye, eye, clip).unwrap();
        let bounds_at = |x| {
            MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
                Affine3A::from_translation(Vec3::new(x, 0.0, 0.0)),
            )
            .unwrap()
        };
        assert!(visible.unbounded.is_reached());
        assert!(graph.mesh_is_drawn(102, &visible, Some(&bounds_at(0.0)), None));
        assert!(!visible.unbounded.intersects(Some(&bounds_at(8.0))));
        assert!(graph.mesh_is_drawn(102, &visible, Some(&bounds_at(8.0)), None));
        let away = graph
            .visibility_volumes(eye, eye, clip_from_local() * Mat4::from_translation(-eye))
            .unwrap();
        assert!(!away.unbounded.is_reached());
        let outside_volumes = bounds_at(0.0);
        assert!(
            away.rooms
                .iter()
                .all(|room| !room.intersects(Some(&outside_volumes)))
        );
        assert!(graph.mesh_is_drawn(102, &away, Some(&outside_volumes), None));
    }

    #[test]
    fn only_verified_outside_ownership_requires_an_outside_route() {
        let connection = authored_database();
        insert_room(&connection, 1, 7, [0.0; 3]);
        insert_room(&connection, 2, 7, [0.0, 50.0, 0.0]);
        connection
            .execute_batch(
                "INSERT INTO room_portals(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,
                half_x,half_z,room_a,room_b) VALUES(10,7,0,25,0,0,0,0,4,4,1,2);
             INSERT INTO [references](id,cell_id,base_form_id) VALUES(100,7,77),(101,7,78);
             INSERT INTO room_reference_rules VALUES(100,7,'auto',NULL),(101,7,'auto',NULL);
             INSERT INTO room_model_bounds VALUES(78,0,0,-80,1);",
            )
            .unwrap();
        let graph = load_room_graph(&connection, 7).unwrap();
        let view = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        let center = Vec3::new(0.0, 0.0, -80.0);
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(center - Vec3::ONE, center + Vec3::ONE),
            Affine3A::IDENTITY,
        )
        .unwrap();
        assert!(view.rooms[0].intersects(Some(&bounds)));
        assert!(!view.unbounded.is_reached());
        let authored = graph.authored.as_ref().unwrap();
        for (id, expected) in [(100, true), (101, false)] {
            let owners =
                authored.initial_owners(id, Affine3A::IDENTITY, &graph.rooms, &graph.portals);
            assert_eq!(
                assignment::draws(&owners, &graph.portals, &view, Some(&bounds)),
                expected
            );
        }
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
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .is_none()
        );
        assert!(
            graph
                .visibility_volumes(
                    Vec3::new(0.0, 0.0, -20.0),
                    Vec3::new(0.0, 0.0, -20.0),
                    clip_from_local()
                )
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
            occluders: HashMap::new(),
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
    fn projected_eye_uses_retail_rotation_rows_for_its_extents() {
        use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

        // Keep the eye in the portal plane to isolate the two native row comparisons.
        for (pitch, yaw, sideways, up, admitted) in [
            (0.0, 0.0, 260.0, 64.0, true),
            (0.0, FRAC_PI_2, 260.0, 64.0, true),
            (0.0, FRAC_PI_4, 55.0, 64.0, true),
            (0.0, FRAC_PI_4, 57.0, 64.0, false),
            (0.0, 0.0, 0.0, 85.0, false),
            // At 45° pitch the displacement along the height contributes to the width check.
            (FRAC_PI_4, 0.0, 0.0, 55.0, true),
            (FRAC_PI_4, 0.0, 0.0, 57.0, false),
        ] {
            let portal = Portal::from_creation(
                1,
                [0.0; 3],
                [pitch, 0.0, yaw],
                [56.0, 84.0],
                [PortalSide::Room(0), PortalSide::Room(1)],
            );
            let width = (portal.corners[1] - portal.corners[0]).normalize();
            let height = (portal.corners[3] - portal.corners[0]).normalize();
            let eye = width * sideways + height * up;
            let view = PortalView {
                eye,
                clip: Mat4::IDENTITY,
                near_plane: Vec4::new(0.0, 1.0, 0.0, -eye.y),
            };
            assert_eq!(
                portal.admits_projected_eye(&view),
                admitted,
                "pitch {pitch}, yaw {yaw}, sideways {sideways}, up {up}",
            );
        }
    }

    #[test]
    fn katariah_drawable_sphere_keeps_only_the_retail_wall_control() {
        let db = authored_database();
        db.execute_batch(
            "INSERT INTO room_bounds VALUES
                 (0x6EE1E,7,-660,1152,768,0,0,3.14159274,528,256,168,NULL,NULL,0),
                 (0x6EE4F,7,-980.000122,1912,768,0,0,3.14159274,207.999847,504,168,NULL,NULL,0),
                 (0x6EE50,7,20,1912,768,0,0,3.14159274,792,504,168,NULL,NULL,0);
             INSERT INTO room_portals VALUES
                 (0x6EF37,7,-508,1408,684,0,0,3.14159274,56,84,0x6EE1E,0x6EE50,0,0),
                 (0x6EF38,7,-772.000305,1792.785522,680.664917,0,0,4.71238947,
                   51.797180,80.664856,0x6EE50,0x6EE4F,0,0);
             INSERT INTO \"references\"(id,cell_id,base_form_id,pos_x,pos_y,pos_z,rot_z) VALUES
                 (0xC4A65,7,0x98594,-772,1405,608,1.57079637),
                 (0xC4A50,7,0x98594,-909.900024,1412.900024,608,1.57079637);
             INSERT INTO room_reference_rules VALUES
                 (0xC4A65,7,'auto',NULL),(0xC4A50,7,'auto',NULL);
             INSERT INTO room_model_bounds VALUES(0x98594,0,128,0,181.057663);",
        )
        .unwrap();
        #[derive(serde::Deserialize)]
        struct Capture {
            position_creation: [f32; 3],
            yaw_degrees: f32,
            pitch_degrees_down: f32,
            vertical_fov_degrees: f32,
            dimensions_pixels: [u32; 2],
            near_plane_creation_units: f32,
        }
        #[derive(serde::Deserialize)]
        struct Receipt {
            retail_capture: Capture,
            live_shape: [f32; 4],
        }
        let receipt: Receipt = serde_json::from_str(include_str!(
            "../../../docs/research/evidence/room-review-20261117/katariah-retail-wall-control.json"
        ))
        .unwrap();
        let capture = receipt.retail_capture;
        let eye = Vec3::from_array(shared::coordinates::creation_to_runtime_vector(
            capture.position_creation,
        ));
        let camera = GlobalTransform::from(Transform::from_translation(eye).with_rotation(
            crate::shots::shot_camera_rotation(capture.yaw_degrees, capture.pitch_degrees_down),
        ));
        let scene = scene_with(GlobalTransform::IDENTITY, camera);
        let mut app = scene.app;
        let world = app.world_mut();
        let loaded = world.resource::<RoomCullingState>().graphs[&7].root;
        world.resource_mut::<RoomCullingState>().insert_graph(
            7,
            loaded,
            load_room_graph(&db, 7).unwrap(),
        );
        let mut camera = world.entity_mut(scene.camera);
        let [width, height] = capture.dimensions_pixels;
        let aspect_ratio = width as f32 / height as f32;
        camera.insert((
            RoomCullingNear::SKYRIM,
            Projection::Perspective(PerspectiveProjection {
                fov: capture.vertical_fov_degrees.to_radians(),
                aspect_ratio,
                near: capture.near_plane_creation_units,
                ..default()
            }),
        ));
        let drawables = [scene.near, scene.far];
        for (mesh, id, pos) in [
            (scene.near, 0xC4A65, Vec3::new(-772.0, 608.0, -1405.0)),
            (scene.far, 0xC4A50, Vec3::new(-909.9, 608.0, -1412.9)),
        ] {
            let reference = world.get::<ChildOf>(mesh).unwrap().parent();
            let placed = Transform::from_translation(pos).with_rotation(Quat::from_array(
                shared::coordinates::creation_euler_to_runtime_quaternion([
                    0.0,
                    0.0,
                    std::f32::consts::FRAC_PI_2,
                ]),
            ));
            world
                .entity_mut(reference)
                .insert((FormId(id), placed, GlobalTransform::from(placed)));
            world.entity_mut(mesh).insert((
                Mesh3d::default(),
                Aabb::from_min_max(
                    Vec3::new(-3.7247002, -128.0, 0.0),
                    Vec3::new(3.7247002, 128.0, 256.0),
                ),
                GlobalTransform::from(placed).mul_transform(Transform::from_rotation(
                    Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
                )),
            ));
        }
        for (with_sphere, doorway_present) in [(false, true), (true, true), (true, false)] {
            let world = app.world_mut();
            if !doorway_present {
                db.execute("DELETE FROM room_portals WHERE ref_id=0x6EF37", [])
                    .unwrap();
                world.resource_mut::<RoomCullingState>().insert_graph(
                    7,
                    loaded,
                    load_room_graph(&db, 7).unwrap(),
                );
            }
            for entity in drawables {
                if with_sphere {
                    world.entity_mut(entity).insert(bevy::gltf::GltfMeshExtras {
                        value: serde_json::json!({
                            "mudcrabDrawableSphere": {"center": [0, 0, 128], "radius": receipt.live_shape[3]}
                        }).to_string(),
                    });
                }
            }
            *world
                .get_mut::<VisibleEntities>(scene.camera)
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>()) = drawables.to_vec();
            world.run_schedule(PostUpdate);
            assert_eq!(
                mesh_list(&app, scene.camera),
                if with_sphere && doorway_present {
                    vec![scene.near]
                } else {
                    Vec::new()
                },
                "drawable sphere {with_sphere}, doorway present {doorway_present}",
            );
        }
    }

    #[test]
    fn turning_at_a_fixed_eye_selects_the_room_at_the_retail_near_distance() {
        let root = Transform::from_xyz(-80.0, 40.0, 120.0)
            .with_rotation(Quat::from_rotation_y(0.8))
            .with_scale(Vec3::splat(2.0));
        let mut scene = scene_with(root.into(), GlobalTransform::IDENTITY);
        let db = authored_database();
        insert_room(&db, 1, 7, [50.0, 0.0, 0.0]);
        insert_room(&db, 2, 7, [-50.0, 0.0, 0.0]);
        db.execute_batch(
            "UPDATE room_bounds SET half_x=50,half_y=50,half_z=50;
            INSERT INTO \"references\"(id,cell_id,base_form_id) VALUES(1,7,99),(2,7,99);
            INSERT INTO room_reference_rules VALUES(1,7,'target',1),(2,7,'target',2);",
        )
        .unwrap();
        let mut graph = load_room_graph(&db, 7).unwrap();
        // A doorway above the view keeps the two starting-room outcomes distinguishable.
        graph.portals = vec![side_doorway(10, Vec3::Y * 45.0, 2.0, [0, 1])];
        for mesh in [scene.near, scene.far] {
            let world = scene.app.world_mut();
            let reference = world.get::<ChildOf>(mesh).unwrap().parent();
            world
                .entity_mut(reference)
                .insert((Transform::IDENTITY, GlobalTransform::from(root)));
        }
        let root_entity = scene.app.world().resource::<RoomCullingState>().graphs[&7].root;
        scene
            .app
            .world_mut()
            .resource_mut::<RoomCullingState>()
            .insert_graph(7, root_entity, graph);
        let eye = root.transform_point(Vec3::X * 3.5);
        for (yaw, expected) in [
            (90.0_f32, scene.near),
            (345.0, scene.near),
            (315.0, scene.far),
            (270.0, scene.far),
            (345.0, scene.near),
            (90.0, scene.near),
        ] {
            let (sin, cos) = yaw.to_radians().sin_cos();
            let forward = root.rotation * Vec3::new(sin, 0.0, -cos);
            let camera = Transform::from_translation(eye).looking_to(forward, Vec3::Y);
            let mut view = scene.app.world_mut().entity_mut(scene.camera);
            view.insert((GlobalTransform::from(camera), RoomCullingNear::SKYRIM));
            *view
                .get_mut::<VisibleEntities>()
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>()) = vec![scene.near, scene.far];
            run_cull(&mut scene.app);
            assert_eq!(mesh_list(&scene.app, scene.camera), [expected], "yaw {yaw}");
        }
    }

    #[test]
    fn the_world_camera_keeps_visible_geometry_at_a_legacy_room_seam() {
        let db = authored_database();
        db.execute_batch(
            "INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z,flags)
             VALUES(1,7,0,2.5,0,0,0,0,5,2.5,5,128),(2,7,0,16.75,0,0,0,0,5,3.25,5,128);
             INSERT INTO room_portals VALUES(10,7,50,9,0,0,0,0,1,1,1,2,0,0);
             INSERT INTO \"references\"(id,cell_id,base_form_id,pos_y) VALUES(1,7,99,3),(2,7,99,16);
             INSERT INTO room_reference_rules VALUES(1,7,'target',1),(2,7,'target',2);
             INSERT INTO reference_rooms VALUES(1,1,1),(2,2,1);",
        ).unwrap();
        let unique = load_room_graph(&db, 7).unwrap();
        db.execute_batch(
            "CREATE TABLE portal_rows AS SELECT * FROM room_portals;
            DROP TABLE room_portals; ALTER TABLE portal_rows RENAME TO room_portals;
            INSERT INTO room_portals SELECT * FROM room_portals;",
        )
        .unwrap();
        let multirow = load_room_graph(&db, 7).unwrap();
        db.execute(
            "DELETE FROM room_graph_stats WHERE name='assignment_version'",
            [],
        )
        .unwrap();
        db.execute("DELETE FROM room_portals WHERE rowid=2", [])
            .unwrap();
        let legacy = load_room_graph(&db, 7).unwrap();

        let mut scene = scene();
        scene.app.world_mut().despawn(scene.camera);
        crate::app::add_test_world_camera(&mut scene.app);
        scene.app.world_mut().run_schedule(Startup);
        let world = scene.app.world_mut();
        let camera = world
            .query_filtered::<Entity, With<StreamingCamera>>()
            .single(world)
            .unwrap();
        world
            .entity_mut(camera)
            .insert(GlobalTransform::from_xyz(0.0, 0.0, 1.0));
        for (mesh, z) in [(scene.near, -3.0), (scene.far, -16.0)] {
            let placement = GlobalTransform::from_xyz(0.0, 0.0, z);
            let reference = world.get::<ChildOf>(mesh).unwrap().parent();
            world.entity_mut(reference).insert(placement);
            world.entity_mut(mesh).insert((
                Aabb::from_min_max(Vec3::splat(-0.1), Vec3::splat(0.1)),
                placement,
            ));
        }
        let root = world.resource::<RoomCullingState>().graphs[&7].root;
        for (mode, graph, expected) in [
            ("legacy", legacy, vec![scene.near, scene.far]),
            ("multirow", multirow, vec![scene.near, scene.far]),
            ("unique authored", unique, vec![scene.far]),
        ] {
            let world = scene.app.world_mut();
            world
                .resource_mut::<RoomCullingState>()
                .insert_graph(7, root, graph);
            *world
                .get_mut::<VisibleEntities>(camera)
                .unwrap()
                .get_mut(TypeId::of::<Mesh3d>()) = vec![scene.near, scene.far];
            run_cull(&mut scene.app);
            assert_eq!(mesh_list(&scene.app, camera), expected, "{mode}");
        }
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
    fn overlap_only_reference_shares_a_bounded_portal_only_when_it_touches_the_aperture() {
        let mut scene = scene();
        {
            let mut state = scene.app.world_mut().resource_mut::<RoomCullingState>();
            let graph = &mut state.graphs.get_mut(&7).unwrap().graph;
            graph.supports_unbounded = true;
            graph.rooms[0].half.z = 5.0;
            graph.rooms[1].center.z = -20.0;
            graph.rooms[1].half.z = 5.0;
            graph.portals[0] = doorway(10, Vec3::new(0.0, 0.0, -10.0), 3.0, [0, 1]);
            for membership in graph.membership.values_mut() {
                membership.contained.clear();
                membership.overlapping = vec![1];
            }
        }
        // Both pivots lie in the gap between the rooms. Both aggregates extend into room 1,
        // but only the wider one reaches the opening. Their children sit beside the aperture.
        for (mesh, half_width) in [(scene.near, 2.0), (scene.far, 0.5)] {
            let reference = scene.app.world().get::<ChildOf>(mesh).unwrap().parent();
            let half = Vec3::new(half_width, 1.0, 5.0);
            scene.app.world_mut().entity_mut(reference).insert((
                ExpectedModelBounds::new(-half, half).unwrap(),
                GlobalTransform::from_translation(Vec3::new(4.0, 0.0, -12.0)),
            ));
            scene.app.world_mut().entity_mut(mesh).insert((
                Aabb::from_min_max(-Vec3::splat(0.25), Vec3::splat(0.25)),
                GlobalTransform::from_translation(Vec3::new(4.0, 0.0, -8.0)),
            ));
        }
        run_cull(&mut scene.app);
        let drawn = mesh_list(&scene.app, scene.camera);
        assert!(drawn.contains(&scene.near));
        assert!(!drawn.contains(&scene.far));
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
    fn stale_fifo_slots_skip_unchanged_portal_clips() {
        let db = authored_database();
        for (id, y) in [(1, 0.0), (2, 60.0), (3, 120.0)] {
            insert_room(&db, id, 7, [0.0, y, 0.0]);
        }
        let mut graph = load_room_graph(&db, 7).unwrap();
        // Three separate apertures reach B before its first scan. All miss B's only exit.
        graph.portals = vec![
            doorway(1, Vec3::new(-5.0, 0.0, -40.0), 0.5, [0, 1]),
            doorway(2, Vec3::new(0.0, 0.0, -40.0), 0.5, [0, 1]),
            doorway(3, Vec3::new(5.0, 0.0, -40.0), 0.5, [0, 1]),
            doorway(4, Vec3::new(1000.0, 0.0, -100.0), 0.5, [1, 2]),
        ];
        volume::PORTAL_CLIP_ATTEMPTS.set(0);
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, [true, true, false]);
        let attempts = volume::PORTAL_CLIP_ATTEMPTS.get();
        assert!(
            attempts <= 6,
            "three entrances and three exit tests suffice, got {attempts}"
        );
    }

    #[test]
    fn interleaved_arrival_uses_the_rooms_existing_fifo_slot() {
        let db = authored_database();
        for id in 1..=5_u16 {
            insert_room(&db, u32::from(id), 7, [0.0, f32::from(id - 1) * 40.0, 0.0]);
        }
        let mut graph = load_room_graph(&db, 7).unwrap();
        // A queues B, C, B in that order. B sends its left path to D on the first
        // visit. C then sends a right path to B before B's second slot is served.
        graph.portals = vec![
            doorway(10, Vec3::new(-5.0, 0.0, -20.0), 1.0, [0, 1]),
            doorway(11, Vec3::new(5.0, 0.0, -20.0), 1.0, [0, 2]),
            doorway(12, Vec3::new(-5.0, 0.0, -20.0), 1.0, [0, 1]),
            doorway(13, Vec3::new(-10.0, 0.0, -40.0), 3.0, [1, 3]),
            doorway(14, Vec3::new(10.0, 0.0, -40.0), 3.0, [2, 1]),
            doorway(15, Vec3::new(15.0, 0.0, -60.0), 3.0, [1, 3]),
            doorway(16, Vec3::new(0.0, 0.0, -70.0), 24.0, [3, 4]),
        ];
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        let markers = [-20.0, 20.0].map(|x| {
            let bounds = MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
                Affine3A::from_translation(Vec3::new(x, 0.0, -80.0)),
            )
            .unwrap();
            visible.rooms[4].intersects(Some(&bounds))
        });
        assert_eq!(
            markers,
            [true, true],
            "D must carry both paths through its exit to E"
        );
    }

    #[test]
    fn authored_exit_carries_all_paths_available_at_its_first_successful_crossing() {
        let connection = authored_database();
        for id in 1..=5 {
            connection.execute(
                "INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z,flags)
                 VALUES(?1,7,0,?2,0,0,0,0,10,10,10,128)",
                rusqlite::params![id, (id - 1) * 40],
            ).unwrap();
        }
        for long_route_first in [false, true] {
            connection.execute("DELETE FROM room_portals", []).unwrap();
            // A sees C through the left aperture and B through the right. B also reaches C.
            // C has a wide exit to D, and a right-only exit to E.
            let ac = if long_route_first { 11 } else { 10 };
            let ab = if long_route_first { 10 } else { 11 };
            for (id, from, to, x, depth, width) in [
                (ac, 1, 3, -5, 20, 1),
                (ab, 1, 2, 5, 20, 1),
                (12, 2, 3, 10, 40, 3),
                (13, 3, 4, 0, 60, 20),
                (14, 3, 5, 15, 60, 2),
            ] {
                connection.execute(
                    "INSERT INTO room_portals(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_z,room_a,room_b,unbounded_a,unbounded_b)
                     VALUES(?1,7,?2,?3,0,0,0,0,?4,3,?5,?6,0,0)",
                    rusqlite::params![id, x, depth, width, from, to],
                ).unwrap();
            }
            let mut graph = load_room_graph(&connection, 7).unwrap();
            let visible = graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .unwrap();
            let markers = |visible: &CellVisibility, room: usize| {
                [-20.0, 0.0, 20.0].map(|x| {
                    let bounds = MeshBounds::transformed(
                        &Aabb::from_min_max(-Vec3::splat(0.1), Vec3::splat(0.1)),
                        Affine3A::from_translation(Vec3::new(x, 0.0, -80.0)),
                    )
                    .unwrap();
                    visible.rooms[room].intersects(Some(&bounds))
                })
            };
            assert_eq!(
                markers(&visible, 2),
                [true, false, true],
                "C merges both paths"
            );
            assert_eq!(
                markers(&visible, 3),
                [true, false, long_route_first],
                "D uses C at crossing time"
            );
            assert_eq!(
                markers(&visible, 4),
                [false, false, true],
                "a missed exit can succeed later"
            );

            // This plane casts an active shadow beyond the wide exit, clear of both markers.
            let disjoint = occlusion::Occluder::from_creation(
                20,
                3,
                [0.0, 70.0, 0.0],
                [0.0; 3],
                [2.0, 0.0, 2.0],
                1.0,
                PortalSide::Room(2),
            )
            .unwrap();
            let view = occlusion::View::new(Vec3::ZERO, clip_from_local()).unwrap();
            assert!(disjoint.shadow_in(&view).is_some());
            graph.add_occluder(disjoint);
            let disjoint = graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .unwrap();
            assert_eq!(markers(&disjoint, 3), markers(&visible, 3));
            graph.occluders.clear();

            // A shadow in C covers only the left route. Its speculative crossing must leave
            // the wide exit available when the clear right-hand contribution reaches C.
            graph.add_occluder(
                occlusion::Occluder::from_creation(
                    20,
                    3,
                    [-12.5, 50.0, 0.0],
                    [0.0; 3],
                    [4.0, 0.0, 4.0],
                    1.0,
                    PortalSide::Room(2),
                )
                .unwrap(),
            );
            let shadowed = graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .unwrap();
            assert_eq!(markers(&shadowed, 3), [false, false, true]);

            // E is reached after C's shadow changes the wide exit's visit state.
            // Overflow must replay the portal walk to recover the same paths as the clear walk.
            for id in 100..100 + MAX_VIEW_OCCLUDERS as u32 {
                graph.add_occluder(
                    occlusion::Occluder::from_creation(
                        id,
                        3,
                        [1000.0, 70.0, 0.0],
                        [0.0; 3],
                        [2.0, 0.0, 2.0],
                        1.0,
                        PortalSide::Room(4),
                    )
                    .unwrap(),
                );
            }
            let overflow = graph
                .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
                .unwrap();
            assert_eq!(markers(&overflow, 3), markers(&visible, 3));
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
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(graph.mesh_is_drawn(100, &volumes, Some(&bounds_at(-6.0)), None));
        assert!(graph.mesh_is_drawn(100, &volumes, Some(&bounds_at(6.0)), None));
        assert!(
            !graph.mesh_is_drawn(100, &volumes, Some(&bounds_at(0.0)), None),
            "the gap between paths stays hidden"
        );
        graph.links.push((0, 1));
        let volumes = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
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
        connection.execute_batch(
            "CREATE TABLE room_graph_cells (cell_id INTEGER PRIMARY KEY, complete INTEGER NOT NULL);
             INSERT INTO room_graph_cells VALUES (7,1);",
        ).unwrap();
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
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
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
            occluders: HashMap::new(),
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
        assert!(quiet.with_room_graphs(None, false).is_quiet());
        let mut state = RoomCullingState::new(PathBuf::new());
        assert!(quiet.with_room_graphs(Some(&state), false).is_quiet());
        AsyncComputeTaskPool::get_or_init(Default::default);
        state.pending.insert(
            7,
            AsyncComputeTaskPool::get().spawn(future::pending::<LoadResult>()),
        );
        let pending = quiet.with_room_graphs(Some(&state), false);
        assert!(quiet.with_room_graphs(Some(&state), true).is_quiet());
        assert_eq!(pending.pending_room_graphs, 1);
        assert!(!pending.is_quiet(), "a pending read must hold the capture");
        state.pending.clear();
        state.failed.insert(7);
        let failed = quiet.with_room_graphs(Some(&state), false);
        assert!(quiet.with_room_graphs(Some(&state), true).is_quiet());
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
    fn a_close_orthographic_portal_crosses_only_if_it_reaches_the_eye_plane() {
        let clip = Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 100.0, 15.0);
        let view = PortalView::new(Vec3::ZERO, Vec3::ZERO, clip);
        let volume = VisibilityVolume::camera(clip).unwrap();
        for (center_z, rotation, crosses) in [
            (3.0, Quat::IDENTITY, false),
            (0.0005, Quat::IDENTITY, false),
            (-3.0, Quat::IDENTITY, true),
            (
                0.5,
                Quat::from_rotation_y(std::f32::consts::FRAC_PI_4),
                true,
            ),
        ] {
            let mut aperture = doorway(10, Vec3::ZERO, 2.0, [0, 1]);
            for corner in &mut aperture.corners {
                *corner = rotation * *corner + Vec3::Z * center_z;
            }
            assert!(aperture.distance_to(Vec3::ZERO) < PORTAL_OPEN_DISTANCE);
            for authored in [false, true] {
                assert_eq!(
                    volume.cross_portal(&aperture, &view, authored).is_some(),
                    crosses,
                    "authored={authored}, center z={center_z}",
                );
            }
        }
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
            occluders: HashMap::new(),
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

    /// A sees B through a narrow doorway; C inherits that aperture through an implicit B-C edge.
    fn overlap_scene(c_center: Vec3) -> RoomGraph {
        let mut rooms = vec![
            room(1, Vec3::ZERO, 10.0),
            room(2, Vec3::new(0.0, 0.0, -30.0), 10.0),
            room(3, c_center, 12.0),
        ];
        let mut portals = vec![doorway(10, Vec3::new(0.0, 0.0, -10.0), 1.0, [0, 1])];
        // A portal on C prevents the legacy portal-less-room whole-cell fallback.
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
            occluders: HashMap::new(),
            rooms,
        }
    }

    #[test]
    fn an_implicit_edge_preserves_the_authored_aperture_for_mesh_tests() {
        let mut graph = overlap_scene(Vec3::new(22.0, 0.0, -34.0));
        assert!(graph.overlaps.contains(&(1, 2)), "B and C overlap");
        graph.membership.insert(
            42,
            Membership {
                contained: vec![2],
                overlapping: vec![],
            },
        );
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(visible.rooms[2].is_reached());
        for (x, drawn) in [(22.0, false), (0.0, true)] {
            let bounds = MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(0.5), Vec3::splat(0.5)),
                Affine3A::from_translation(Vec3::new(x, 0.0, -34.0)),
            )
            .unwrap();
            assert_eq!(
                graph.mesh_is_drawn(42, &visible, Some(&bounds), None),
                drawn,
                "the arriving doorway still bounds meshes, including overhanging ones"
            );
        }
    }

    #[test]
    fn an_overlapping_room_partly_inside_the_region_is_reached() {
        let graph = overlap_scene(Vec3::new(4.0, 0.0, -34.0));
        assert!(graph.overlaps.contains(&(1, 2)));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true, false]);
    }

    #[test]
    fn an_implicit_edge_does_not_admit_meshes_behind_the_camera() {
        let mut rooms = vec![
            room(1, Vec3::ZERO, 10.0),
            room(2, Vec3::new(0.0, 0.0, 15.0), 10.0),
        ];
        let mut portals = Vec::new();
        // Both rooms carry a portal to avoid the legacy whole-cell fallback.
        park_portals(&mut rooms, &mut portals, &[0, 1]);
        let mut graph = RoomGraph {
            unresolved_link_rooms: HashSet::new(),
            incomplete: false,
            overlaps: overlap_links(&rooms),
            portals,
            rooms,
            ..Default::default()
        };
        graph.membership.insert(
            42,
            Membership {
                contained: vec![1],
                overlapping: vec![],
            },
        );
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::ONE, Vec3::ONE),
            Affine3A::from_translation(Vec3::new(0.0, 0.0, 15.0)),
        )
        .unwrap();
        assert!(visible.rooms[1].is_reached());
        assert!(!graph.mesh_is_drawn(42, &visible, Some(&bounds), None));
    }

    /// Implicit-edge fixtures with parked portals to prevent the legacy whole-cell fallback.
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

    #[test]
    fn implicit_edges_keep_in_frustum_meshes_overhanging_the_room_box() {
        let mut graph = overlap_only(vec![
            room(1, Vec3::ZERO, 6.0),
            room(2, Vec3::new(0.0, 0.0, -18.0), 5.0),
        ]);
        assert_eq!(
            graph.overlaps,
            vec![(0, 1)],
            "the boxes have a seven-unit gap"
        );
        graph.membership.insert(
            42,
            Membership {
                contained: vec![1],
                overlapping: vec![],
            },
        );
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(Vec3::splat(-0.2), Vec3::splat(0.2)),
            Affine3A::from_translation(Vec3::new(8.0, 0.0, -18.0)),
        )
        .unwrap();
        assert!(
            VisibilityVolume::camera(clip_from_local())
                .unwrap()
                .intersects(&bounds)
        );
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(visible.rooms[1].is_reached());
        assert!(
            graph.mesh_is_drawn(42, &visible, Some(&bounds), None),
            "a child's geometry may extend beyond its pivot-containing room"
        );
    }

    fn small(ref_id: u32, center: Vec3, half: f32) -> Room {
        room(ref_id, center, half)
    }

    /// An inferred chain carries the camera volume even when the room rectangles do not overlap.
    #[test]
    fn implicit_edge_chains_preserve_the_arriving_volume() {
        let graph = overlap_only(vec![
            small(1, Vec3::ZERO, 6.0),
            small(2, Vec3::new(0.0, 0.0, -18.0), 5.0),
            small(3, Vec3::new(16.0, 0.0, -22.0), 5.0),
        ]);
        assert!(graph.overlaps.contains(&(0, 1)) && graph.overlaps.contains(&(1, 2)));
        assert!(!graph.overlaps.contains(&(0, 2)));
        let reached = reached_rooms(&graph, Vec3::ZERO, clip_from_local()).unwrap();
        assert_eq!(reached, vec![true, true, true, false]);
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
            occluders: HashMap::new(),
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
            occluders: HashMap::new(),
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

    /// An authored doorway still excludes backdrop meshes after an implicit edge.
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
        let mut graph = graph;
        graph.membership.insert(
            42,
            Membership {
                contained: vec![2],
                overlapping: vec![],
            },
        );
        let visible = graph
            .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip_from_local())
            .unwrap();
        assert!(visible.rooms[2].is_reached());
        let bounds = MeshBounds::transformed(
            &Aabb::from_min_max(-Vec3::splat(100.0), Vec3::splat(100.0)),
            Affine3A::from_translation(graph.rooms[2].center),
        )
        .unwrap();
        assert!(
            !graph.mesh_is_drawn(42, &visible, Some(&bounds), None),
            "the backdrop mesh remains outside the authored doorway"
        );
    }

    /// Parked portals prevent whole-cell fallback without narrowing the inferred links.
    #[test]
    fn rooms_with_portals_preserve_implicit_edges_without_box_clipping() {
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
        assert_eq!(&reached[..3], [true, true, true]);
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
