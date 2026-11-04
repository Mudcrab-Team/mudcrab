//! Flatten validated distant stock into affine-only, logically owned draws.

use super::partial_stock::PartialStock;
use crate::cluster::{
    flat_source::{FlatSourceOwner, FlatSourcePrimitive, FlatSourcePrimitives},
    source_identity::{SourcePrimitive, SourcePrimitiveTransform},
};
use crate::world::reference_state::ReferenceVisibilityGate;
use bevy::{
    animation::AnimationTargetId,
    app::{PropagateOver, PropagateSet},
    camera::visibility::{RenderLayers, VisibilitySystems},
    ecs::{entity_disabling::Disabled, query::Allow},
    prelude::*,
    transform::TransformSystems,
};
use std::{collections::HashSet, sync::Arc};

#[derive(Resource, Default)]
struct PendingFlat(HashSet<Entity>);

#[derive(Component)]
struct FlatPose {
    chain: Arc<[Transform]>,
    authored_visibility: Visibility,
}

pub(super) fn install(app: &mut App) {
    app.init_resource::<PendingFlat>()
        .add_systems(Update, queue_validated.after(super::track_asset_readiness))
        .add_systems(
            PostUpdate,
            flatten_ready
                .after(TransformSystems::Propagate)
                .before(VisibilitySystems::VisibilityPropagate),
        )
        .add_systems(
            PostUpdate,
            sync_flat
                .after(PropagateSet::<RenderLayers>::default())
                .after(VisibilitySystems::VisibilityPropagate)
                .before(VisibilitySystems::CheckVisibility),
        );
}

fn queue_validated(
    mut removed: RemovedComponents<super::PendingAssetProfile>,
    roots: Query<(), With<PartialStock>>,
    mut pending: ResMut<PendingFlat>,
) {
    for entity in removed.read() {
        if roots.contains(entity) {
            pending.0.insert(entity);
        }
    }
}

pub(super) fn queue(world: &mut World, root: Entity) {
    if let Some(mut pending) = world.get_resource_mut::<PendingFlat>() {
        pending.0.insert(root);
    }
}

fn flatten_ready(world: &mut World) {
    let roots = std::mem::take(&mut world.resource_mut::<PendingFlat>().0);
    for root in roots {
        if world
            .get::<PartialStock>(root)
            .is_none_or(|partial| partial.preserve_hierarchy)
            || world.get::<super::AssetFailed>(root).is_some()
            || world.get::<super::PendingAssetProfile>(root).is_some()
        {
            continue;
        }
        flatten(world, root);
    }
}

fn descendants(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = world
        .get::<Children>(root)
        .map(|children| children.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    while let Some(entity) = stack.pop() {
        out.push(entity);
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
    out
}

fn pose_chain(world: &World, entity: Entity, root: Entity) -> Option<(Vec<Transform>, Visibility)> {
    let mut chain = Vec::new();
    let mut visibility = Visibility::Inherited;
    let mut current = entity;
    let mut visited = HashSet::new();
    while current != root {
        if !visited.insert(current) {
            return None;
        }
        let node = world.get_entity(current).ok()?;
        if node.contains::<super::PendingAssetProfile>()
            || node.contains::<super::AssetFailed>()
            || node.contains::<AnimationTargetId>()
            || node.contains::<AnimationPlayer>()
            || node.contains::<crate::billboard::Billboard>()
            || node.contains::<bevy::mesh::skinning::SkinnedMesh>()
            || node.contains::<bevy::mesh::morph::MorphWeights>()
            || node.contains::<bevy::mesh::morph::MeshMorphWeights>()
            || node.get::<bevy::gltf::GltfExtras>().is_some_and(|extras| {
                crate::billboard::BillboardMode::from_extras(&extras.value).is_some()
            })
        {
            return None;
        }
        if visibility == Visibility::Inherited {
            // A reference gate is temporary, never part of the baked flat pose.
            visibility = node
                .get::<ReferenceVisibilityGate>()
                .and_then(|gate| gate.0)
                .or_else(|| node.get::<Visibility>().copied())
                .unwrap_or_default();
        }
        chain.push(*node.get::<Transform>()?);
        current = node.get::<ChildOf>()?.parent();
    }
    chain.reverse();
    Some((chain, visibility))
}

/// Called only after ordinary validation and material swaps. Detaching the existing
/// mesh entity preserves every final material handle and render flag without a copy.
pub(super) fn flatten(world: &mut World, root: Entity) {
    let nodes = descendants(world, root);
    let mut plans = Vec::new();
    let mut retained_ancestors = HashSet::new();
    for &entity in &nodes {
        if world.get::<Mesh3d>(entity).is_none() {
            continue;
        }
        let plan = (|| {
            world.get::<SourcePrimitive>(entity)?;
            let (chain, visibility) = pose_chain(world, entity, root)?;
            let model = *world.get::<SourcePrimitiveTransform>(entity)?;
            let global = *world.get::<GlobalTransform>(entity)?;
            Some((entity, chain, visibility, model, global))
        })();
        if let Some(plan) = plan {
            plans.push(plan);
        } else {
            let mut current = entity;
            while current != root && retained_ancestors.insert(current) {
                let Some(parent) = world.get::<ChildOf>(current) else {
                    break;
                };
                current = parent.parent();
            }
        }
    }
    // Capture every pose before detaching a mesh that is another mesh's ancestor.
    // A retained dynamic draw still needs all of its transform ancestors.
    for (entity, chain, visibility, model, global) in plans {
        if retained_ancestors.contains(&entity) {
            continue;
        };
        // Required components are inserted when Mesh3d is inserted. Removing
        // Transform afterwards is supported and excludes this draw from both
        // simple and hierarchical transform propagation, including shear.
        world
            .entity_mut(entity)
            .insert(PropagateOver::<RenderLayers>::default())
            .remove::<(ChildOf, Transform)>()
            .remove::<bevy::app::Inherited<RenderLayers>>()
            .insert((
                global,
                FlatSourceOwner(root),
                FlatSourcePrimitive {
                    model_matrix: model.0,
                    expected_global: global,
                },
                FlatPose {
                    chain: chain.into(),
                    authored_visibility: visibility,
                },
            ));
    }
    // Leaves first. Dynamic branches retain their hierarchy; an animated source
    // is never silently frozen.
    for entity in nodes.into_iter().rev() {
        if world.get::<Mesh3d>(entity).is_none()
            && world.get::<super::PendingAssetProfile>(entity).is_none()
            && world
                .get::<Children>(entity)
                .is_none_or(|children| children.is_empty())
            && world.get::<FlatSourceOwner>(entity).is_none()
        {
            world.despawn(entity);
        }
    }
}

type FlatRoots<'w, 's> = Query<
    'w,
    's,
    (
        Ref<'static, GlobalTransform>,
        Ref<'static, FlatSourcePrimitives>,
        Ref<'static, InheritedVisibility>,
        Option<Ref<'static, RenderLayers>>,
    ),
    (
        Allow<Disabled>,
        Or<(
            Changed<GlobalTransform>,
            Changed<InheritedVisibility>,
            Changed<RenderLayers>,
            Changed<FlatSourcePrimitives>,
        )>,
    ),
>;
type FlatDraws<'w, 's> = Query<
    'w,
    's,
    (
        &'static FlatPose,
        &'static mut GlobalTransform,
        &'static mut FlatSourcePrimitive,
        &'static mut Visibility,
        &'static mut InheritedVisibility,
        Option<&'static ReferenceVisibilityGate>,
        Option<&'static mut RenderLayers>,
    ),
    Without<FlatSourcePrimitives>,
>;

#[cfg(test)]
#[derive(Default)]
struct FlatSyncObservation {
    roots: u64,
    pose_roots: u64,
    visibility_roots: u64,
    layer_roots: u64,
    membership_roots: u64,
    members: u64,
    pose_computations: u64,
    global_writes: u64,
    proof_writes: u64,
}

#[cfg(test)]
impl FlatSyncObservation {
    fn record(self) {
        if self.roots == 0 {
            return;
        }
        #[cfg(test)]
        for (name, amount) in [
            ("flat_stock/sync_roots", self.roots),
            ("flat_stock/sync_pose_roots", self.pose_roots),
            ("flat_stock/sync_visibility_roots", self.visibility_roots),
            ("flat_stock/sync_layer_roots", self.layer_roots),
            ("flat_stock/sync_membership_roots", self.membership_roots),
            ("flat_stock/sync_members", self.members),
            ("flat_stock/sync_pose_computations", self.pose_computations),
            ("flat_stock/sync_global_writes", self.global_writes),
            ("flat_stock/sync_proof_writes", self.proof_writes),
        ] {
            #[cfg(test)]
            crate::work_counters::record_count(name, amount);
        }
    }
}

fn sync_flat(roots: FlatRoots, mut draws: FlatDraws, mut commands: Commands) {
    #[cfg(test)]
    let mut observation = FlatSyncObservation::default();
    for (root, members, inherited, layers) in &roots {
        #[cfg(test)]
        {
            observation.roots += 1;
        }
        #[cfg(test)]
        {
            observation.pose_roots += u64::from(root.is_changed());
        }
        #[cfg(test)]
        {
            observation.visibility_roots += u64::from(inherited.is_changed());
        }
        #[cfg(test)]
        {
            observation.layer_roots +=
                u64::from(layers.as_ref().is_some_and(|value| value.is_changed()));
        }
        #[cfg(test)]
        {
            observation.membership_roots += u64::from(members.is_changed());
        }
        let layers = layers.as_deref();
        for entity in members.iter() {
            #[cfg(test)]
            {
                observation.members += 1;
            }
            let Ok((
                pose,
                mut global,
                mut proof,
                mut visibility,
                mut inherited_draw,
                reference_gate,
                draw_layers,
            )) = draws.get_mut(entity)
            else {
                continue;
            };
            #[cfg(test)]
            {
                observation.pose_computations += 1;
            }
            let next = pose
                .chain
                .iter()
                .fold(*root, |parent, local| parent.mul_transform(*local));
            if *global != next {
                #[cfg(test)]
                {
                    observation.global_writes += 1;
                }
                *global = next;
            }
            if proof.expected_global != next {
                #[cfg(test)]
                {
                    observation.proof_writes += 1;
                }
                proof.expected_global = next;
            }
            let visible = reference_gate.is_none()
                && match pose.authored_visibility {
                    Visibility::Visible => true,
                    Visibility::Hidden => false,
                    Visibility::Inherited => inherited.get(),
                };
            let explicit = if visible {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
            if *visibility != explicit {
                *visibility = explicit;
            }
            let next_inherited = if visible {
                InheritedVisibility::VISIBLE
            } else {
                InheritedVisibility::HIDDEN
            };
            if *inherited_draw != next_inherited {
                *inherited_draw = next_inherited;
            }
            if let Some(layers) = layers {
                if let Some(mut current) = draw_layers {
                    if *current != *layers {
                        *current = layers.clone();
                    }
                } else {
                    commands.entity(entity).insert(layers.clone());
                }
            }
        }
    }
    #[cfg(test)]
    {
        observation.record();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{camera::visibility::VisibilityPlugin, transform::TransformPlugin};

    #[test]
    fn explicit_flat_visibility_cannot_override_reference_disable() {
        use crate::world::{
            components::FormId,
            reference_state::{self, EnableCatalog, ReferenceEnableState, SetReferenceEnabled},
        };
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE \"references\" (id INTEGER PRIMARY KEY); INSERT INTO \"references\" VALUES (7);").unwrap();
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            TransformPlugin,
            VisibilityPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>()
        .add_systems(
            PostUpdate,
            sync_flat
                .after(VisibilitySystems::VisibilityPropagate)
                .before(VisibilitySystems::CheckVisibility),
        );
        reference_state::install(&mut app);
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(Arc::new(EnableCatalog::read(&connection).unwrap()));
        let root = app
            .world_mut()
            .spawn((FormId(7), Transform::default(), Visibility::Inherited))
            .id();
        let draw = app
            .world_mut()
            .spawn((
                Mesh3d::default(),
                FlatSourceOwner(root),
                Visibility::Visible,
                FlatSourcePrimitive {
                    model_matrix: Mat4::IDENTITY,
                    expected_global: GlobalTransform::default(),
                },
                FlatPose {
                    chain: Arc::from([Transform::default()]),
                    authored_visibility: Visibility::Visible,
                },
            ))
            .remove::<Transform>()
            .id();
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(draw),
            Some(&Visibility::Visible)
        );
        for enabled in [false, true, false, true] {
            app.world_mut().trigger(SetReferenceEnabled {
                reference: 7,
                enabled,
            });
            app.update();
            assert_eq!(
                app.world().get::<Visibility>(draw),
                Some(if enabled {
                    &Visibility::Visible
                } else {
                    &Visibility::Hidden
                })
            );
            assert_eq!(
                app.world().get::<InheritedVisibility>(draw).unwrap().get(),
                enabled
            );
        }
    }

    #[test]
    fn nested_meshes_flatten_together_but_keep_billboard_ancestry() {
        for billboard in [false, true] {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, TransformPlugin))
                .add_systems(PostUpdate, sync_flat.after(TransformSystems::Propagate));
            let root = app
                .world_mut()
                .spawn((
                    Transform::from_scale(Vec3::new(2.0, 3.0, 1.0)),
                    Visibility::Inherited,
                    InheritedVisibility::VISIBLE,
                ))
                .id();
            let parent_pose = Transform::from_rotation(Quat::from_rotation_z(0.7));
            let child_pose = Transform::from_xyz(3.0, 2.0, 1.0);
            let tag = |ordinal| SourcePrimitive {
                scene_index: 0,
                ordinal,
                mesh_index: 0,
                primitive_index: ordinal,
                matches_default_scene: true,
            };
            let parent = app
                .world_mut()
                .spawn((
                    Mesh3d::default(),
                    parent_pose,
                    Visibility::Inherited,
                    tag(0),
                    SourcePrimitiveTransform(parent_pose.to_matrix()),
                    ChildOf(root),
                ))
                .id();
            let child = app
                .world_mut()
                .spawn((
                    Mesh3d::default(),
                    child_pose,
                    Visibility::Inherited,
                    tag(1),
                    SourcePrimitiveTransform(parent_pose.to_matrix() * child_pose.to_matrix()),
                    ChildOf(parent),
                ))
                .id();
            if billboard {
                app.world_mut()
                    .entity_mut(child)
                    .insert(crate::billboard::Billboard {
                        mode: crate::billboard::BillboardMode::FaceCamera,
                        authored: Quat::IDENTITY,
                    });
            }
            app.update();
            let before = *app.world().get::<GlobalTransform>(child).unwrap();
            flatten(app.world_mut(), root);
            app.update();
            assert_eq!(*app.world().get::<GlobalTransform>(child).unwrap(), before);
            for entity in [parent, child] {
                assert_eq!(app.world().get::<Transform>(entity).is_some(), billboard);
                assert_eq!(app.world().get::<ChildOf>(entity).is_some(), billboard);
            }
            app.world_mut()
                .get_mut::<Transform>(root)
                .unwrap()
                .translation += Vec3::X * 100.0;
            app.update();
            let expected = app
                .world()
                .get::<GlobalTransform>(root)
                .unwrap()
                .mul_transform(parent_pose)
                .mul_transform(child_pose);
            assert_eq!(
                *app.world().get::<GlobalTransform>(child).unwrap(),
                expected
            );
            app.world_mut().despawn(root);
            assert!(app.world().get_entity(child).is_err());
        }
    }

    #[test]
    fn affine_draw_preserves_shear_and_required_transform_stays_removed() {
        for scale in [Vec3::splat(2.0), Vec3::new(2.0, 3.0, 0.5)] {
            let mut app = App::new();
            app.add_plugins((
                MinimalPlugins,
                bevy::asset::AssetPlugin::default(),
                TransformPlugin,
                VisibilityPlugin,
                bevy::app::HierarchyPropagatePlugin::<RenderLayers>::new(PostUpdate),
            ))
            .init_asset::<Mesh>()
            .init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>()
            .add_systems(
                PostUpdate,
                sync_flat
                    .after(PropagateSet::<RenderLayers>::default())
                    .after(VisibilitySystems::VisibilityPropagate)
                    .before(VisibilitySystems::CheckVisibility),
            );
            let root = app
                .world_mut()
                .spawn((
                    Transform::from_xyz(10.0, -20.0, 30.0).with_scale(scale),
                    Visibility::Inherited,
                    RenderLayers::layer(2),
                    bevy::app::Propagate(RenderLayers::layer(2)),
                ))
                .id();
            let pivot_pose = Transform::from_rotation(Quat::from_rotation_z(0.61));
            let pivot = app
                .world_mut()
                .spawn((pivot_pose, Visibility::Inherited, ChildOf(root)))
                .id();
            let local =
                Transform::from_xyz(7.0, 2.0, -4.0).with_rotation(Quat::from_rotation_x(0.29));
            let draw = app
                .world_mut()
                .spawn((
                    Mesh3d::default(),
                    local,
                    Visibility::Inherited,
                    ChildOf(pivot),
                ))
                .id();
            app.update();
            let expected = *app.world().get::<GlobalTransform>(draw).unwrap();
            let matrix = pivot_pose.to_matrix() * local.to_matrix();
            app.world_mut()
                .entity_mut(draw)
                .insert(PropagateOver::<RenderLayers>::default())
                .remove::<(ChildOf, Transform)>()
                .remove::<bevy::app::Inherited<RenderLayers>>()
                .insert((
                    FlatSourceOwner(root),
                    FlatSourcePrimitive {
                        model_matrix: matrix,
                        expected_global: expected,
                    },
                    FlatPose {
                        chain: vec![pivot_pose, local].into(),
                        authored_visibility: Visibility::Inherited,
                    },
                ));
            for _ in 0..4 {
                app.update();
                assert!(app.world().get::<Transform>(draw).is_none());
                assert!(app.world().get::<ChildOf>(draw).is_none());
                assert_eq!(*app.world().get::<GlobalTransform>(draw).unwrap(), expected);
                assert_eq!(
                    app.world().get::<RenderLayers>(draw),
                    Some(&RenderLayers::layer(2))
                );
            }
            app.world_mut()
                .get_mut::<Transform>(root)
                .unwrap()
                .translation += Vec3::new(1024.0, 0.0, -512.0);
            app.update();
            let rebased = app
                .world()
                .get::<GlobalTransform>(root)
                .unwrap()
                .mul_transform(pivot_pose)
                .mul_transform(local);
            assert_eq!(*app.world().get::<GlobalTransform>(draw).unwrap(), rebased);
            assert_eq!(
                app.world().get::<RenderLayers>(draw),
                Some(&RenderLayers::layer(2))
            );
            app.world_mut().entity_mut(root).insert((
                Visibility::Hidden,
                bevy::app::Propagate(RenderLayers::layer(3)),
            ));
            app.update();
            assert!(!app.world().get::<InheritedVisibility>(draw).unwrap().get());
            assert_eq!(
                app.world().get::<RenderLayers>(draw),
                Some(&RenderLayers::layer(3))
            );
            app.world_mut().despawn(root);
            assert!(app.world().get_entity(draw).is_err());
        }
    }
}
