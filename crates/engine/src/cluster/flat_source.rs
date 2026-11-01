//! Source proof for unparented stock draws; ownership never propagates transforms.
use bevy::{prelude::*, world_serialization::WorldAssetRoot};

/// Lifetime and source ancestry only. A flat draw never inherits its owner's pose.
#[derive(Component)]
#[relationship(relationship_target = FlatSourcePrimitives)]
pub(crate) struct FlatSourceOwner(pub Entity);

#[derive(Component, Default)]
#[relationship_target(relationship = FlatSourceOwner, linked_spawn)]
pub(crate) struct FlatSourcePrimitives(Vec<Entity>);

impl FlatSourcePrimitives {
    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.0.iter().copied()
    }
}

/// Exact validated model-space baseline and world-space affine, without TRS decomposition.
#[derive(Component, Clone)]
pub(crate) struct FlatSourcePrimitive {
    pub model_matrix: Mat4,
    pub expected_global: GlobalTransform,
}

/// Generated subsets have no source path; retain the original scene's identity.
#[derive(Component)]
pub(crate) struct OriginalStockScene(pub(crate) Handle<WorldAsset>);

pub(super) fn logical_parent(
    entity: Entity,
    parents: &Query<&ChildOf>,
    owners: &Query<&FlatSourceOwner>,
) -> Option<Entity> {
    parents
        .get(entity)
        .ok()
        .map(ChildOf::parent)
        .or_else(|| owners.get(entity).ok().map(|owner| owner.0))
}

pub(super) fn scene_path_matches(
    asset: Option<&WorldAssetRoot>,
    original: Option<&OriginalStockScene>,
    expected_path: &str,
) -> bool {
    original
        .map(|original| &original.0)
        .or_else(|| asset.map(|asset| &asset.0))
        .is_none_or(|scene| {
            scene.path().is_some_and(|path| {
                path.path().to_string_lossy().replace('\\', "/") == expected_path
            })
        })
}

pub(super) type FlatPrimitiveState<'a> = (
    Option<&'a FlatSourcePrimitive>,
    Option<&'a FlatSourceOwner>,
    Option<&'a GlobalTransform>,
    Has<Transform>,
    Has<ChildOf>,
    Option<&'a bevy::gltf::GltfExtras>,
    Has<crate::billboard::Billboard>,
    Has<bevy::mesh::skinning::SkinnedMesh>,
    Has<bevy::mesh::morph::MorphWeights>,
    Has<bevy::mesh::morph::MeshMorphWeights>,
);
pub(super) type FlatPrimitiveDomain = Or<(With<FlatSourcePrimitive>, With<FlatSourceOwner>)>;

/// The caller must separately validate the owning reference's certificate root.
pub(super) fn model_matrix(
    state: bevy::ecs::query::QueryItem<'_, '_, FlatPrimitiveState<'_>>,
    root: Entity,
) -> Option<Mat4> {
    let (metadata, owner, global, transform, child, extras, billboard, skin, morph, mesh_morph) =
        state;
    let metadata = metadata?;
    if owner?.0 != root
        || transform
        || child
        || *global? != metadata.expected_global
        || !metadata.expected_global.affine().is_finite()
        || !metadata.model_matrix.is_finite()
        || billboard
        || skin
        || morph
        || mesh_morph
        || extras.is_some_and(|extras| {
            crate::billboard::BillboardMode::from_extras(&extras.value).is_some()
        })
    {
        return None;
    }
    Some(metadata.model_matrix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::SystemState;

    #[test]
    fn owner_despawn_despawns_flat_primitives_without_transform_parenting() {
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let first = world.spawn(FlatSourceOwner(owner)).id();
        let second = world.spawn(FlatSourceOwner(owner)).id();
        assert_eq!(
            world
                .get::<FlatSourcePrimitives>(owner)
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(world.get::<Children>(owner).is_none());
        world.despawn(owner);
        assert!(!world.entities().contains_spawned(first));
        assert!(!world.entities().contains_spawned(second));
    }

    #[test]
    fn child_of_precedes_flat_owner_in_logical_ancestry() {
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let parent = world.spawn_empty().id();
        let primitive = world.spawn((FlatSourceOwner(owner), ChildOf(parent))).id();
        let mut state: SystemState<(Query<&ChildOf>, Query<&FlatSourceOwner>)> =
            SystemState::new(&mut world);
        let (parents, owners) = state.get(&world).unwrap();
        assert_eq!(logical_parent(primitive, &parents, &owners), Some(parent));
        world.entity_mut(primitive).remove::<ChildOf>();
        let (parents, owners) = state.get(&world).unwrap();
        assert_eq!(logical_parent(primitive, &parents, &owners), Some(owner));
    }
}
