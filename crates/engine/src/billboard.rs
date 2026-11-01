//! Skyrim's billboard nodes: model parts that turn to face the camera, such as a hearth's flame
//! cards (`FireplaceWood01Burning`'s `Flames` node).
//!
//! A NIF marks them with an `NiBillboardNode`, which the converter publishes as node extras,
//! `{"openSkyrim": {"billboard": "<mode>"}}`, the mode being nif.xml's `BillboardMode` name in
//! camelCase, or its numeric enum value. Without the turn a card
//! keeps its authored angle and is seen edge-on or from behind from most places in a room.
//!
//! Three behaviours cover the modes:
//! * **Face the camera** (`alwaysFaceCamera`, `rigidFaceCamera`): the node takes the camera's
//!   orientation. A card authored in the node's XY plane is parallel to the image plane.
//! * **Face the eye** (`alwaysFaceCenter`, `rigidFaceCenter`): the node's +Z points toward the
//!   camera position, using the camera's up direction. An off-axis card is no longer parallel
//!   to the image plane. Coincident positions retain the camera orientation.
//! * **Turn about up** (`rotateAboutUp`, `rotateAboutUp2`, `bsRotateAboutUp`): the node keeps its
//!   authored tilt and turns about the world's up axis until its facing axis (node-local Z) points
//!   at the camera across the ground plane. Flames stay upright however the camera looks down.
//!
//! The camera is the player's, [`StreamingCamera`]. Secondary views see the same orientations.
//!
//! Limitation: billboard ancestors must have finite, strictly positive uniform local scales.
//! Negative/nonuniform (or zero) ancestor scale can reflect or shear the parent affine, so a
//! local quaternion alone cannot in general produce the requested world orientation. Such
//! hierarchies retain the authored local rotation and warn once; they resume turning when
//! the ancestor scales become supported. Translation and scale are never rewritten.

use std::collections::{HashMap, HashSet};

use bevy::{
    ecs::lifecycle::{Insert, Remove},
    gltf::GltfExtras,
    prelude::*,
    transform::{TransformSystems, helper::TransformHelper},
};

use crate::world::components::StreamingCamera;

/// How a billboard node turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillboardMode {
    FaceCamera,
    FaceCenter,
    TurnAboutUp,
}

impl BillboardMode {
    /// The behaviour for a published mode name; `None` for a name this engine does not know.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "alwaysFaceCamera" | "rigidFaceCamera" => Some(Self::FaceCamera),
            "alwaysFaceCenter" | "rigidFaceCenter" => Some(Self::FaceCenter),
            "rotateAboutUp" | "rotateAboutUp2" | "bsRotateAboutUp" => Some(Self::TurnAboutUp),
            _ => None,
        }
    }

    /// The mode a node's glTF extras publish, if any.
    pub fn from_extras(extras: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(extras).ok()?;
        let mode = value.get("openSkyrim")?.get("billboard")?;
        let parsed = if let Some(name) = mode.as_str() {
            Self::from_name(name)
        } else {
            match mode.as_u64() {
                Some(0 | 2) => Some(Self::FaceCamera),
                Some(3 | 4) => Some(Self::FaceCenter),
                Some(1 | 5 | 9) => Some(Self::TurnAboutUp),
                _ => None,
            }
        };
        if parsed.is_none() {
            warn_once!(%mode, "unsupported authored billboard mode; keeping authored transform");
        }
        parsed
    }
}

/// A node that turns to face the camera, with the local rotation it was authored with.
#[derive(Component, Debug, Clone, Copy)]
pub struct Billboard {
    pub mode: BillboardMode,
    pub authored: Quat,
}

/// A node's local transform as the model authored it: a billboard's turn toward the camera undone,
/// any other node's transform as it is.
///
/// Streaming's strict check compares a spawned model's bounds with the bounds the converter
/// computed from the authored NIF. A billboard's turn follows the camera, so without this the
/// measured bounds of a hearth or campfire - and whether it passes - would depend on where the
/// camera stood when the check ran.
pub fn authored_transform(local: &Transform, billboard: Option<&Billboard>) -> Transform {
    match billboard {
        Some(billboard) => local.with_rotation(billboard.authored),
        None => *local,
    }
}

pub struct BillboardPlugin;

impl Plugin for BillboardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BillboardDiscovery>()
            .init_resource::<BillboardOrder>()
            .add_observer(billboard_extras_inserted)
            .add_observer(billboard_transform_inserted)
            .add_observer(billboard_input_removed)
            .add_observer(billboard_membership_inserted)
            .add_observer(billboard_membership_removed)
            .add_observer(billboard_parent_inserted)
            .add_observer(billboard_parent_removed);
        // One-time bootstrap for installation after spawning. No tick-filtered query runs
        // during settled updates. Count bootstrap visits as well as journal entity lookups.
        let world = app.world_mut();
        let existing: Vec<_> = world
            .query_filtered::<Entity, With<GltfExtras>>()
            .iter(world)
            .collect();
        let mut discovery = world.resource_mut::<BillboardDiscovery>();
        #[cfg(test)]
        {
            discovery.visited_entities += existing.len();
        }
        for entity in existing {
            discovery.pending.entry(entity).or_insert(None);
        }
        app.add_systems(
            PostUpdate,
            find_billboards
                .before(bevy::app::AnimationSystems)
                .before(TransformSystems::Propagate),
        )
        .add_systems(
            PostUpdate,
            turn_billboards
                .after(find_billboards)
                .after(bevy::app::AnimationSystems)
                .before(TransformSystems::Propagate),
        );
    }
}

/// Input lifecycle only: mutable animation/camera transforms do not enqueue discovery.
/// Some(rotation) records an explicitly inserted Transform before animation can change it.
#[derive(Resource, Default)]
struct BillboardDiscovery {
    pending: HashMap<Entity, Option<Quat>>,
    #[cfg(test)]
    visited_entities: usize,
}

/// Retain parent-first traversal storage until membership or ancestry changes.
#[derive(Resource)]
struct BillboardOrder {
    entities: Vec<Entity>,
    ancestors: HashSet<Entity>,
    dirty: bool,
    #[cfg(test)]
    rebuilds: usize,
}

impl Default for BillboardOrder {
    fn default() -> Self {
        Self {
            entities: Vec::new(),
            ancestors: HashSet::new(),
            dirty: true,
            #[cfg(test)]
            rebuilds: 0,
        }
    }
}

fn billboard_membership_inserted(_event: On<Insert, Billboard>, mut order: ResMut<BillboardOrder>) {
    order.dirty = true;
}

fn billboard_membership_removed(_event: On<Remove, Billboard>, mut order: ResMut<BillboardOrder>) {
    order.dirty = true;
}

fn billboard_parent_inserted(event: On<Insert, ChildOf>, mut order: ResMut<BillboardOrder>) {
    if order.ancestors.contains(&event.entity) {
        order.dirty = true;
    }
}

fn billboard_parent_removed(event: On<Remove, ChildOf>, mut order: ResMut<BillboardOrder>) {
    if order.ancestors.contains(&event.entity) {
        order.dirty = true;
    }
}

fn billboard_extras_inserted(
    event: On<Insert, GltfExtras>,
    mut discovery: ResMut<BillboardDiscovery>,
) {
    discovery.pending.entry(event.entity).or_insert(None);
}

fn billboard_transform_inserted(
    event: On<Insert, Transform>,
    nodes: Query<(&Transform, Has<GltfExtras>, Has<Billboard>)>,
    mut discovery: ResMut<BillboardDiscovery>,
) {
    // Include the observer's direct lookup, even when this is an ordinary transform.
    #[cfg(test)]
    {
        discovery.visited_entities += 1;
    }
    if let Ok((transform, has_extras, has_billboard)) = nodes.get(event.entity)
        && (has_extras || has_billboard)
    {
        discovery
            .pending
            .insert(event.entity, Some(transform.rotation));
    }
}

fn billboard_input_removed(
    event: On<Remove, (GltfExtras, Transform)>,
    mut discovery: ResMut<BillboardDiscovery>,
) {
    discovery.pending.entry(event.entity).or_insert(None);
}

fn find_billboards(
    mut commands: Commands,
    mut discovery: ResMut<BillboardDiscovery>,
    mut nodes: Query<(Option<&GltfExtras>, &mut Transform, Option<&Billboard>)>,
) {
    if discovery.pending.is_empty() {
        return;
    }
    // Split fields to retain HashMap storage while draining; count every get attempt,
    // including missing/despawned/ordinary entities, not merely successful discoveries.
    let BillboardDiscovery {
        pending,
        #[cfg(test)]
        visited_entities,
    } = &mut *discovery;
    for (entity, inserted_rotation) in pending.drain() {
        #[cfg(test)]
        {
            *visited_entities += 1;
        }
        let Ok((extras, mut transform, billboard)) = nodes.get_mut(entity) else {
            commands.entity(entity).try_remove::<Billboard>();
            continue;
        };
        let authored = inserted_rotation
            .or_else(|| billboard.map(|billboard| billboard.authored))
            .unwrap_or(transform.rotation);
        if let Some(mode) = extras.and_then(|extras| BillboardMode::from_extras(&extras.value)) {
            commands
                .entity(entity)
                .try_insert(Billboard { mode, authored });
        } else if billboard.is_some() {
            transform.rotation = authored;
            commands.entity(entity).try_remove::<Billboard>();
        }
    }
}

/// The world rotation a billboard takes: `authored` is its world rotation as placed, `to_camera`
/// the direction from it to the camera and `camera` the camera's world rotation, all in Bevy's
/// Y-up world.
pub fn billboard_rotation(
    mode: BillboardMode,
    authored: Quat,
    to_camera: Vec3,
    camera: Quat,
) -> Quat {
    match mode {
        BillboardMode::FaceCamera => camera,
        BillboardMode::FaceCenter => {
            let Some(facing) = to_camera.try_normalize() else {
                return camera;
            };
            let up = camera * Vec3::Y;
            let right = up.cross(facing).try_normalize().unwrap_or_else(|| {
                // At the up-axis singularity, keep the camera's right axis
                // projected into the card plane instead of producing NaNs.
                let right = camera * Vec3::X;
                (right - facing * right.dot(facing)).normalize()
            });
            Quat::from_mat3(&Mat3::from_cols(right, facing.cross(right), facing))
        }
        BillboardMode::TurnAboutUp => {
            let facing = authored * Vec3::Z;
            let from = Vec2::new(facing.x, facing.z);
            let to = Vec2::new(to_camera.x, to_camera.z);
            if from.length_squared() < 1e-8 || to.length_squared() < 1e-8 {
                return authored;
            }
            // The turn about +Y that carries `from` onto `to` in the XZ plane. A rotation about +Y
            // by `a` takes (x, z) to (x cos a + z sin a, -x sin a + z cos a), so the angle is
            // measured from `to` back to `from`.
            let angle = to.angle_to(from);
            Quat::from_rotation_y(angle) * authored
        }
    }
}

/// Compose the rotation from local quaternions only after proving that the entire
/// parent chain has no reflection or anisotropic scale. Never decompose a sheared affine.
fn supported_parent_rotation(
    parent: Entity,
    parents: &Query<&ChildOf>,
    transforms: &Query<&mut Transform>,
) -> Option<Quat> {
    let mut rotation = Quat::IDENTITY;
    for ancestor in std::iter::once(parent).chain(parents.iter_ancestors(parent)) {
        let transform = transforms.get(ancestor).ok()?;
        let scale = transform.scale;
        if !scale.is_finite() || scale.x <= 0.0 || scale.x != scale.y || scale.x != scale.z {
            return None;
        }
        rotation = transform.rotation * rotation;
    }
    Some(rotation)
}

/// Read current local transforms before propagation; last frame's GlobalTransform
/// cannot describe a moving camera or parent. Process parent billboards first.
fn turn_billboards(
    camera: Query<Entity, With<StreamingCamera>>,
    billboards: Query<(Entity, &Billboard, Option<&ChildOf>)>,
    parents: Query<&ChildOf>,
    mut transforms: ParamSet<(TransformHelper, Query<&mut Transform>)>,
    mut order: ResMut<BillboardOrder>,
) {
    if order.dirty {
        order.entities.clear();
        order
            .entities
            .extend(billboards.iter().map(|(entity, _, _)| entity));
        order
            .entities
            .sort_unstable_by_key(|entity| parents.iter_ancestors(*entity).count());
        order.ancestors.clear();
        for (entity, _, _) in &billboards {
            order
                .ancestors
                .extend(std::iter::once(entity).chain(parents.iter_ancestors(entity)));
        }
        order.dirty = false;
        #[cfg(test)]
        {
            order.rebuilds += 1;
        }
    }
    let Ok(camera) = camera.single() else {
        return;
    };
    let Ok(camera) = transforms.p0().compute_global_transform(camera) else {
        return;
    };
    let (_, camera_rotation, camera_position) = camera.to_scale_rotation_translation();
    for &entity in &order.entities {
        let Ok((_, billboard, child_of)) = billboards.get(entity) else {
            continue;
        };
        let parent_rotation = child_of.map_or(Some(Quat::IDENTITY), |child_of| {
            supported_parent_rotation(child_of.parent(), &parents, &transforms.p1())
        });
        let Some(parent_rotation) = parent_rotation else {
            warn_once!(
                "billboard ancestor scale is unsupported; retaining authored local rotation (positive uniform scales required)"
            );
            if let Ok(mut transform) = transforms.p1().get_mut(entity)
                && transform.rotation != billboard.authored
            {
                transform.rotation = billboard.authored;
            }
            continue;
        };
        let Ok(global) = transforms.p0().compute_global_transform(entity) else {
            continue;
        };
        let world = billboard_rotation(
            billboard.mode,
            parent_rotation * billboard.authored,
            camera_position - global.translation(),
            camera_rotation,
        );
        let local = parent_rotation.inverse() * world;
        let mut locals = transforms.p1();
        let Ok(mut transform) = locals.get_mut(entity) else {
            continue;
        };
        if transform.rotation != local {
            transform.rotation = local;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_first_order_rebuilds_only_for_membership_and_ancestry_changes() {
        let mut app = App::new();
        app.init_resource::<BillboardOrder>()
            .add_observer(billboard_membership_inserted)
            .add_observer(billboard_membership_removed)
            .add_observer(billboard_parent_inserted)
            .add_observer(billboard_parent_removed)
            .add_systems(Update, turn_billboards);
        app.world_mut().spawn((
            StreamingCamera,
            Transform::from_xyz(5.0, 1.0, 4.0),
            GlobalTransform::from_translation(Vec3::new(5.0, 1.0, 4.0)),
        ));
        let card = Billboard {
            mode: BillboardMode::FaceCamera,
            authored: Quat::IDENTITY,
        };
        let child = app.world_mut().spawn((Transform::default(), card)).id();
        let branch = app
            .world_mut()
            .spawn(Transform::default())
            .add_child(child)
            .id();
        let parent = app
            .world_mut()
            .spawn((Transform::default(), card))
            .add_child(branch)
            .id();
        app.update();
        assert_eq!(
            app.world().resource::<BillboardOrder>().entities,
            vec![parent, child]
        );
        let storage = app.world().resource::<BillboardOrder>().entities.as_ptr();
        let unrelated = app.world_mut().spawn(Transform::default()).id();
        for _ in 0..32 {
            let extra = app.world_mut().spawn(Transform::default()).id();
            app.world_mut().entity_mut(unrelated).add_child(extra);
            app.world_mut()
                .get_mut::<Transform>(parent)
                .unwrap()
                .rotate_y(0.1);
            app.update();
            let order = app.world().resource::<BillboardOrder>();
            assert_eq!(
                order.rebuilds, 1,
                "local motion must not rediscover or sort membership"
            );
            assert_eq!(
                order.entities.as_ptr(),
                storage,
                "settled storage is retained"
            );
        }
        let outside = app.world_mut().spawn(Transform::default()).id();
        app.world_mut().entity_mut(outside).add_child(branch);
        app.update();
        assert_eq!(
            app.world().resource::<BillboardOrder>().rebuilds,
            2,
            "a non-billboard ancestor's reparent changes descendant depths"
        );
        app.world_mut().entity_mut(child).add_child(parent);
        app.update();
        assert_eq!(
            app.world().resource::<BillboardOrder>().entities,
            vec![child, parent]
        );
        assert_eq!(app.world().resource::<BillboardOrder>().rebuilds, 3);
        app.world_mut().entity_mut(child).remove::<Billboard>();
        app.update();
        assert_eq!(
            app.world().resource::<BillboardOrder>().entities,
            vec![parent]
        );
        assert_eq!(app.world().resource::<BillboardOrder>().rebuilds, 4);
        app.world_mut().entity_mut(child).insert(card);
        app.update();
        assert_eq!(
            app.world().resource::<BillboardOrder>().entities,
            vec![child, parent]
        );
        assert_eq!(app.world().resource::<BillboardOrder>().rebuilds, 5);
        app.world_mut().entity_mut(branch).despawn();
        app.update();
        assert!(app.world().resource::<BillboardOrder>().entities.is_empty());
        assert_eq!(app.world().resource::<BillboardOrder>().rebuilds, 6);
    }

    #[test]
    fn published_mode_names_keep_camera_center_and_up_behaviours_distinct() {
        for (name, mode) in [
            ("alwaysFaceCamera", BillboardMode::FaceCamera),
            ("rigidFaceCamera", BillboardMode::FaceCamera),
            ("alwaysFaceCenter", BillboardMode::FaceCenter),
            ("rigidFaceCenter", BillboardMode::FaceCenter),
            ("rotateAboutUp", BillboardMode::TurnAboutUp),
            ("rotateAboutUp2", BillboardMode::TurnAboutUp),
            ("bsRotateAboutUp", BillboardMode::TurnAboutUp),
        ] {
            let extras = serde_json::json!({"openSkyrim": {"billboard": name}});
            assert_eq!(
                BillboardMode::from_extras(&extras.to_string()),
                Some(mode),
                "{name}"
            );
        }
        assert_eq!(
            BillboardMode::from_extras(r#"{"openSkyrim":{"billboard":"rotateAboutUp"}}"#),
            Some(BillboardMode::TurnAboutUp)
        );
        assert_eq!(
            BillboardMode::from_name("alwaysFaceCamera"),
            Some(BillboardMode::FaceCamera)
        );
        assert_eq!(
            BillboardMode::from_extras(r#"{"openSkyrim":{"billboard":7}}"#),
            None
        );
        assert_eq!(BillboardMode::from_extras(r#"{"other":1}"#), None);
    }

    #[test]
    fn turning_about_up_points_the_facing_axis_at_the_camera_and_keeps_it_upright() {
        // Authored facing +Z; the camera stands off along +X and above.
        let to_camera = Vec3::new(10.0, 4.0, 0.0);
        let world = billboard_rotation(
            BillboardMode::TurnAboutUp,
            Quat::IDENTITY,
            to_camera,
            Quat::IDENTITY,
        );
        let facing = world * Vec3::Z;
        assert!((facing - Vec3::X).length() < 1e-5, "{facing}");
        assert!(
            ((world * Vec3::Y) - Vec3::Y).length() < 1e-5,
            "stays upright"
        );

        // A tilted card keeps its tilt: only a turn about +Y is added.
        let tilted = Quat::from_rotation_x(0.3);
        let world = billboard_rotation(
            BillboardMode::TurnAboutUp,
            tilted,
            Vec3::new(-5.0, 0.0, 0.0),
            Quat::IDENTITY,
        );
        let facing = world * Vec3::Z;
        let flat = Vec2::new(facing.x, facing.z).normalize();
        assert!((flat - Vec2::new(-1.0, 0.0)).length() < 1e-5, "{facing}");
        assert!(((world * Vec3::X).y).abs() < 1e-5, "no roll is added");
    }

    #[test]
    fn the_authored_transform_undoes_only_a_billboard_turn() {
        let authored = Quat::from_rotation_x(0.3);
        let turned = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0))
            .with_rotation(Quat::from_rotation_y(1.2) * authored)
            .with_scale(Vec3::splat(2.0));
        let billboard = Billboard {
            mode: BillboardMode::TurnAboutUp,
            authored,
        };
        let restored = authored_transform(&turned, Some(&billboard));
        assert_eq!(restored.rotation, authored);
        assert_eq!(restored.translation, turned.translation);
        assert_eq!(restored.scale, turned.scale);
        assert_eq!(authored_transform(&turned, None), turned);
    }

    #[test]
    fn facing_the_camera_takes_its_rotation() {
        let camera = Quat::from_euler(EulerRot::YXZ, 1.0, -0.4, 0.0);
        let world = billboard_rotation(BillboardMode::FaceCamera, Quat::IDENTITY, Vec3::X, camera);
        assert_eq!(world, camera);
    }

    #[test]
    fn center_modes_point_at_the_eye_off_axis_and_follow_camera_roll() {
        let camera = Quat::from_rotation_z(0.4);
        let to_camera = Vec3::new(5.0, 2.0, 8.0);
        for name in ["alwaysFaceCenter", "rigidFaceCenter"] {
            let world = billboard_rotation(
                BillboardMode::from_name(name).unwrap(),
                Quat::from_rotation_y(0.7),
                to_camera,
                camera,
            );
            let facing = to_camera.normalize();
            assert!(((world * Vec3::Z) - facing).length() < 1e-5, "{name}");
            let camera_up = camera * Vec3::Y;
            let projected_up = (camera_up - facing * camera_up.dot(facing)).normalize();
            assert!(((world * Vec3::Y) - projected_up).length() < 1e-5);
            assert!(((world * Vec3::Z) - camera * Vec3::Z).length() > 0.1);
        }
    }

    #[test]
    fn center_modes_handle_the_up_axis_and_coincident_eye() {
        let camera = Quat::from_rotation_z(0.4);
        for to_camera in [camera * Vec3::Y, camera * -Vec3::Y, Vec3::ZERO] {
            let world =
                billboard_rotation(BillboardMode::FaceCenter, Quat::IDENTITY, to_camera, camera);
            assert!(world.is_finite() && world.is_normalized());
            if to_camera == Vec3::ZERO {
                assert_eq!(world, camera);
            } else {
                assert!(((world * Vec3::Z) - to_camera.normalize()).length() < 1e-5);
            }
        }
    }

    #[test]
    fn turning_under_a_rotated_parent_changes_only_the_local_rotation() {
        for mode in [
            BillboardMode::FaceCamera,
            BillboardMode::FaceCenter,
            BillboardMode::TurnAboutUp,
        ] {
            let mut app = App::new();
            app.init_resource::<BillboardOrder>()
                .add_observer(billboard_membership_inserted)
                .add_observer(billboard_membership_removed)
                .add_observer(billboard_parent_inserted)
                .add_observer(billboard_parent_removed)
                .add_systems(Update, turn_billboards);
            let camera_rotation = Quat::from_euler(EulerRot::YXZ, 0.3, -0.2, 0.1);
            let camera_position = Vec3::new(8.0, 4.0, 7.0);
            app.world_mut().spawn((
                StreamingCamera,
                Transform::from_translation(camera_position).with_rotation(camera_rotation),
                GlobalTransform::from(
                    Transform::from_translation(camera_position).with_rotation(camera_rotation),
                ),
            ));
            let parent_rotation = Quat::from_rotation_y(0.7);
            let parent = app
                .world_mut()
                .spawn(Transform::from_rotation(parent_rotation))
                .id();
            let local = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0))
                .with_rotation(Quat::from_rotation_x(0.2))
                .with_scale(Vec3::splat(2.0));
            let global = GlobalTransform::from(Transform::from_rotation(parent_rotation)) * local;
            let expected = billboard_rotation(
                mode,
                parent_rotation * local.rotation,
                camera_position - global.translation(),
                camera_rotation,
            );
            let card = app
                .world_mut()
                .spawn((
                    Billboard {
                        mode,
                        authored: local.rotation,
                    },
                    local,
                    global,
                    ChildOf(parent),
                ))
                .id();
            app.update();
            let turned = app.world().get::<Transform>(card).unwrap();
            let world_rotation = parent_rotation * turned.rotation;
            assert!((world_rotation * Vec3::Z - expected * Vec3::Z).length() < 1e-5);
            assert!((world_rotation * Vec3::Y - expected * Vec3::Y).length() < 1e-5);
            assert_eq!(turned.translation, local.translation);
            assert_eq!(turned.scale, local.scale);
            assert_eq!(
                authored_transform(turned, app.world().get::<Billboard>(card)),
                local
            );
        }
    }
    #[test]
    fn numeric_modes_match_the_published_nif_enum() {
        for (number, mode) in [
            (0, BillboardMode::FaceCamera),
            (1, BillboardMode::TurnAboutUp),
            (2, BillboardMode::FaceCamera),
            (3, BillboardMode::FaceCenter),
            (4, BillboardMode::FaceCenter),
            (5, BillboardMode::TurnAboutUp),
            (9, BillboardMode::TurnAboutUp),
        ] {
            assert_eq!(
                BillboardMode::from_extras(
                    &serde_json::json!({
                        "openSkyrim": {"billboard": number}
                    })
                    .to_string()
                ),
                Some(mode)
            );
        }
        for number in [-1, 6, 7, 8, 10] {
            assert_eq!(
                BillboardMode::from_extras(
                    &serde_json::json!({
                        "openSkyrim": {"billboard": number}
                    })
                    .to_string()
                ),
                None
            );
        }
    }

    #[test]
    fn camera_and_parent_motion_are_used_in_the_same_propagation_frame() {
        let mut app = App::new();
        app.add_plugins((TransformPlugin, BillboardPlugin));
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, Transform::from_xyz(0.0, 0.0, 10.0)))
            .id();
        let parent = app.world_mut().spawn(Transform::default()).id();
        let local = Transform::from_xyz(1.0, 2.0, 3.0).with_rotation(Quat::from_rotation_x(0.2));
        let card = app
            .world_mut()
            .spawn((
                local,
                ChildOf(parent),
                GltfExtras {
                    value: r#"{"openSkyrim":{"billboard":3}}"#.into(),
                },
            ))
            .id();
        app.update();
        let parent_pose =
            Transform::from_xyz(3.0, 0.0, 1.0).with_rotation(Quat::from_rotation_y(0.7));
        let camera_pose =
            Transform::from_xyz(10.0, 4.0, -8.0).with_rotation(Quat::from_rotation_z(0.3));
        *app.world_mut().get_mut::<Transform>(parent).unwrap() = parent_pose;
        *app.world_mut().get_mut::<Transform>(camera).unwrap() = camera_pose;
        app.update();
        let card_position = (GlobalTransform::from(parent_pose) * local).translation();
        let expected = billboard_rotation(
            BillboardMode::FaceCenter,
            parent_pose.rotation * local.rotation,
            camera_pose.translation - card_position,
            camera_pose.rotation,
        );
        let (_, rotation, position) = app
            .world()
            .get::<GlobalTransform>(card)
            .unwrap()
            .to_scale_rotation_translation();
        assert!((position - card_position).length() < 1e-5);
        assert!((rotation * Vec3::Z - expected * Vec3::Z).length() < 1e-5);
        assert!((rotation * Vec3::Y - expected * Vec3::Y).length() < 1e-5);
        assert_eq!(
            authored_transform(
                app.world().get::<Transform>(card).unwrap(),
                app.world().get::<Billboard>(card)
            ),
            local
        );
    }

    #[test]
    fn unknown_published_modes_remain_unsupported() {
        for mode in [
            serde_json::json!("futureMode"),
            serde_json::json!(99),
            serde_json::json!(null),
        ] {
            assert_eq!(
                BillboardMode::from_extras(
                    &serde_json::json!({"openSkyrim": {"billboard": mode}}).to_string()
                ),
                None
            );
        }
    }

    #[test]
    fn unsupported_parent_scales_restore_authored_rotation_without_decomposing_shear() {
        for scale in [
            Vec3::new(-1.0, 1.0, 1.0),
            Vec3::new(2.0, 1.0, 1.0),
            Vec3::ZERO,
        ] {
            for mode in [
                BillboardMode::FaceCamera,
                BillboardMode::FaceCenter,
                BillboardMode::TurnAboutUp,
            ] {
                let mut app = App::new();
                app.init_resource::<BillboardOrder>()
                    .add_observer(billboard_membership_inserted)
                    .add_observer(billboard_membership_removed)
                    .add_observer(billboard_parent_inserted)
                    .add_observer(billboard_parent_removed)
                    .add_systems(Update, turn_billboards);
                app.world_mut()
                    .spawn((StreamingCamera, Transform::from_xyz(8.0, 4.0, 7.0)));
                let grandparent = app.world_mut().spawn(Transform::from_scale(scale)).id();
                // A rotation below nonuniform scale makes the affine parent genuinely sheared.
                let parent = app
                    .world_mut()
                    .spawn((
                        Transform::from_rotation(Quat::from_rotation_y(0.7)),
                        ChildOf(grandparent),
                    ))
                    .id();
                let authored = Quat::from_rotation_x(0.2);
                let local = Transform::from_xyz(1.0, 2.0, 3.0)
                    .with_rotation(Quat::from_rotation_z(1.0))
                    .with_scale(Vec3::splat(2.0));
                let card = app
                    .world_mut()
                    .spawn((Billboard { mode, authored }, local, ChildOf(parent)))
                    .id();
                app.update();
                let turned = app.world().get::<Transform>(card).unwrap();
                assert_eq!(turned.rotation, authored);
                assert_eq!(turned.translation, local.translation);
                assert_eq!(turned.scale, local.scale);
                assert!(turned.rotation.is_finite() && turned.rotation.is_normalized());
                // Returning to supported scale resumes same-frame turning.
                app.world_mut()
                    .get_mut::<Transform>(grandparent)
                    .unwrap()
                    .scale = Vec3::splat(2.0);
                app.update();
                let turned = app.world().get::<Transform>(card).unwrap();
                assert_ne!(turned.rotation, authored);
                assert!(turned.rotation.is_finite() && turned.rotation.is_normalized());
            }
        }
    }
    #[test]
    fn lifecycle_discovery_visits_no_ordinary_extras_on_settled_frames() {
        let mut app = App::new();
        // Bootstrap covers installation after ordinary nodes already exist.
        for _ in 0..2048 {
            app.world_mut().spawn((
                Transform::default(),
                GltfExtras {
                    value: r#"{"ordinary":true}"#.into(),
                },
            ));
        }
        app.add_plugins(BillboardPlugin);
        app.update();
        let visited = app
            .world()
            .resource::<BillboardDiscovery>()
            .visited_entities;
        assert!(visited >= 2048);
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<BillboardDiscovery>()
                .visited_entities,
            visited
        );
        let node = app
            .world_mut()
            .spawn((Transform::default(), GltfExtras { value: "{}".into() }))
            .id();
        app.update();
        assert_eq!(
            app.world()
                .resource::<BillboardDiscovery>()
                .visited_entities,
            visited + 2
        );
        app.world_mut().entity_mut(node).despawn();
        app.update();
        let settled = app
            .world()
            .resource::<BillboardDiscovery>()
            .visited_entities;
        app.update();
        assert_eq!(
            app.world()
                .resource::<BillboardDiscovery>()
                .visited_entities,
            settled
        );
    }

    #[test]
    fn lifecycle_discovery_handles_late_extras_transform_replacement_and_withdrawal() {
        let mut app = App::new();
        app.add_plugins(BillboardPlugin);
        let authored = Quat::from_rotation_x(0.3);
        let node = app
            .world_mut()
            .spawn(Transform::from_rotation(authored))
            .id();
        app.update();
        assert!(app.world().get::<Billboard>(node).is_none());
        let extras = || GltfExtras {
            value: r#"{"openSkyrim":{"billboard":0}}"#.into(),
        };
        app.world_mut().entity_mut(node).insert(extras());
        app.update();
        assert_eq!(
            app.world().get::<Billboard>(node).unwrap().authored,
            authored
        );
        // A camera turn is in-place mutation: rediscovery must not capture it as authored.
        app.world_mut().get_mut::<Transform>(node).unwrap().rotation = Quat::from_rotation_y(1.0);
        app.world_mut().entity_mut(node).insert(GltfExtras {
            value: r#"{"openSkyrim":{"billboard":3}}"#.into(),
        });
        app.update();
        let billboard = app.world().get::<Billboard>(node).unwrap();
        assert_eq!(billboard.mode, BillboardMode::FaceCenter);
        assert_eq!(billboard.authored, authored);
        app.world_mut().entity_mut(node).remove::<GltfExtras>();
        app.update();
        assert!(app.world().get::<Billboard>(node).is_none());
        assert_eq!(
            app.world().get::<Transform>(node).unwrap().rotation,
            authored
        );
        app.world_mut().entity_mut(node).insert(extras());
        app.update();
        let replacement = Quat::from_rotation_z(0.4);
        app.world_mut()
            .entity_mut(node)
            .insert(Transform::from_rotation(replacement));
        app.update();
        assert_eq!(
            app.world().get::<Billboard>(node).unwrap().authored,
            replacement
        );
        app.world_mut()
            .entity_mut(node)
            .insert(GltfExtras { value: "{}".into() });
        app.update();
        assert!(app.world().get::<Billboard>(node).is_none());
        assert_eq!(
            app.world().get::<Transform>(node).unwrap().rotation,
            replacement
        );
        let late_transform = app.world_mut().spawn(extras()).id();
        app.update();
        assert!(app.world().get::<Billboard>(late_transform).is_none());
        app.world_mut()
            .entity_mut(late_transform)
            .insert(Transform::from_rotation(authored));
        app.update();
        assert_eq!(
            app.world()
                .get::<Billboard>(late_transform)
                .unwrap()
                .authored,
            authored
        );
        app.world_mut()
            .entity_mut(late_transform)
            .remove::<Transform>();
        app.update();
        assert!(app.world().get::<Billboard>(late_transform).is_none());
        app.world_mut()
            .entity_mut(late_transform)
            .insert(Transform::from_rotation(replacement));
        app.update();
        assert_eq!(
            app.world()
                .get::<Billboard>(late_transform)
                .unwrap()
                .authored,
            replacement
        );
    }

    #[test]
    fn discovery_captures_authored_rotation_before_animation_systems() {
        fn animate(mut transforms: Query<&mut Transform, With<GltfExtras>>) {
            for mut transform in &mut transforms {
                transform.rotation = Quat::from_rotation_y(1.2);
            }
        }
        let mut app = App::new();
        app.add_plugins(BillboardPlugin)
            .add_systems(PostUpdate, animate.in_set(bevy::app::AnimationSystems));
        let authored = Quat::from_rotation_x(0.3);
        let node = app
            .world_mut()
            .spawn((
                Transform::from_rotation(authored),
                GltfExtras {
                    value: r#"{"openSkyrim":{"billboard":0}}"#.into(),
                },
            ))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<Billboard>(node).unwrap().authored,
            authored
        );
        assert_eq!(
            app.world().get::<Transform>(node).unwrap().rotation,
            Quat::from_rotation_y(1.2)
        );
    }
}
