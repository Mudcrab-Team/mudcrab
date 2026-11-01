//! Structural invalidation for identity caches, including custom default disablers.
//! Callers install terrain tracking for the identity indices they use. In-place component mutations
//! are checked by each consumer's terrain-restricted change-filtered queries.
use bevy::ecs::{component::ComponentId, entity_disabling::DefaultQueryFilters};
use bevy::prelude::*;
use bevy::world_serialization::WorldAssetRoot;

#[derive(Resource, Default)]
pub(crate) struct StructureEpoch {
    pub version: u64,
    disablers: std::collections::HashSet<ComponentId>,
}

impl StructureEpoch {
    pub(crate) fn covered_version(&self, filters: &DefaultQueryFilters) -> Option<u64> {
        filters
            .disabling_ids()
            .all(|id| self.disablers.contains(&id))
            .then_some(self.version)
    }
}

/// Entity-local invalidation shared by the terrain identity and LOD indices.
/// Consumers own independent cursors; an expired cursor requires a fresh snapshot.
#[derive(Message, Clone, Copy)]
pub(crate) struct TerrainNodeChanged(pub Entity);

#[derive(Resource)]
pub(crate) struct TerrainTracking {
    disablers: std::collections::HashSet<ComponentId>,
}

impl TerrainTracking {
    pub(crate) fn covered(&self, filters: &DefaultQueryFilters) -> bool {
        filters
            .disabling_ids()
            .all(|id| self.disablers.contains(&id))
    }
}

/// Install shared terrain invalidation for identity and LOD consumers.
pub(crate) fn install_terrain(world: &mut World) {
    if world.contains_resource::<TerrainTracking>() {
        return;
    }
    let child_of = world.register_component::<ChildOf>();
    let existing_hierarchy = world
        .archetypes()
        .iter()
        .any(|archetype| archetype.contains(child_of) && !archetype.is_empty());
    world.insert_resource(
        super::terrain_identity::TerrainRelationshipChecks::bootstrap(existing_hierarchy),
    );
    // Bevy 0.19 relationship/mod.rs queues the backlink through Commands. Deferring
    // this check preserves that order and prevents normal append inserts looking skipped.
    world.spawn(Observer::new(
        |event: On<Insert, ChildOf>, mut commands: Commands| {
            let child = event.entity;
            commands.queue(move |world: &mut World| {
                super::terrain_identity::check_relationship_backlink(world, child)
            });
        },
    ));
    world.spawn(Observer::new(
        |event: On<Remove, ChildOf>, mut commands: Commands| {
            let child = event.entity;
            commands.queue(move |world: &mut World| {
                super::terrain_identity::check_relationship_backlink(world, child)
            });
        },
    ));
    world.spawn(Observer::new(
        |event: On<Discard, ChildOf>, world: &World, mut commands: Commands| {
            let parent = world.get::<ChildOf>(event.entity).map(ChildOf::parent);
            if let Some(parent) = parent {
                commands.queue(move |world: &mut World| {
                    world
                        .resource_mut::<Messages<TerrainNodeChanged>>()
                        .write(TerrainNodeChanged(parent));
                });
            }
        },
    ));
    // Replacing Children with relationship hooks skipped can remove backlinks without
    // changing ChildOf. Snapshot only this event's former siblings before replacement;
    // raw World access includes disabled parents. Check after queued hooks have run.
    world.spawn(Observer::new(
        |event: On<Discard, Children>, world: &World, mut commands: Commands| {
            let children: Vec<_> = world
                .get::<Children>(event.entity)
                .map(|children| children.iter().collect())
                .unwrap_or_default();
            if children.is_empty() {
                return;
            }
            commands.queue(move |world: &mut World| {
                for child in children {
                    super::terrain_identity::check_relationship_backlink(world, child);
                }
            });
        },
    ));
    let disablers: std::collections::HashSet<_> = world
        .resource::<DefaultQueryFilters>()
        .disabling_ids()
        .collect();
    let mut components = vec![
        world.register_component::<Mesh3d>(),
        world.register_component::<ChildOf>(),
        world.register_component::<Children>(),
        world.register_component::<super::TerrainCoverage>(),
        world.register_component::<super::TerrainWorldspace>(),
        world.register_component::<super::lod::LodChunkRoot>(),
        world.register_component::<super::TerrainSurfaceReady>(),
        world.register_component::<super::TerrainPatch>(),
        world.register_component::<crate::swap_bridge::SwapDrawn>(),
        world.register_component::<Visibility>(),
    ];
    components.extend(disablers.iter().copied());
    components.sort();
    components.dedup();
    bevy::ecs::message::MessageRegistry::register_message::<TerrainNodeChanged>(world);
    world.insert_resource(TerrainTracking { disablers });
    world.spawn(
        Observer::new(
            |event: On<Insert>, mut changes: ResMut<Messages<TerrainNodeChanged>>| {
                changes.write(TerrainNodeChanged(event.entity));
            },
        )
        .with_components(components.clone()),
    );
    world.spawn(
        Observer::new(
            |event: On<Remove>, mut changes: ResMut<Messages<TerrainNodeChanged>>| {
                changes.write(TerrainNodeChanged(event.entity));
            },
        )
        .with_components(components),
    );
}

/// Exact cluster-source lifecycle journal; independent extraction cursor.
/// Each recorded entry carries its exact epoch increment, including unrelated entities.
#[derive(Message, Clone, Copy)]
pub(crate) struct SourceNodeChanged {
    pub entity: Entity,
    pub epoch: u64,
}

pub(crate) fn install(world: &mut World) {
    install_terrain(world);
    if world.contains_resource::<StructureEpoch>() {
        return;
    }
    use crate::{
        cluster::{
            flat_source::{
                FlatSourceOwner, FlatSourcePrimitive, FlatSourcePrimitives, OriginalStockScene,
            },
            source_identity::*,
        },
        world::components::{CellRef, FormId, MeshHandle},
    };
    macro_rules! ids { ($($ty:ty),* $(,)?) => { vec![$(world.register_component::<$ty>()),*] }; }
    let mut components = ids![
        Transform,
        GlobalTransform,
        FlatSourceOwner,
        FlatSourcePrimitive,
        FlatSourcePrimitives,
        OriginalStockScene,
        ChildOf,
        Mesh3d,
        FormId,
        CellRef,
        MeshHandle,
        WorldAssetRoot,
        Visibility,
        InheritedVisibility,
        bevy::camera::visibility::RenderLayers,
        SourcePrimitive,
        SourcePrimitiveMesh,
        SourcePrimitiveTransform,
        SourceStaticModel,
        SourceNodePose,
        crate::world::components::StreamedCellKey,
        super::AssetFailed,
        super::TerrainCoverage,
        super::TerrainWorldspace,
        super::lod::LodChunkRoot,
        MeshMaterial3d<StandardMaterial>,
        bevy::gltf::GltfExtras,
        crate::billboard::Billboard,
        bevy::mesh::skinning::SkinnedMesh,
        bevy::mesh::morph::MorphWeights,
        bevy::animation::AnimationTargetId,
        bevy::mesh::morph::MeshMorphWeights,
        bevy::light::NotShadowCaster,
    ];
    let disablers: std::collections::HashSet<_> = world
        .resource::<DefaultQueryFilters>()
        .disabling_ids()
        .collect();
    components.extend(disablers.iter().copied());
    components.sort();
    components.dedup();
    bevy::ecs::message::MessageRegistry::register_message::<SourceNodeChanged>(world);
    world.insert_resource(StructureEpoch {
        version: 0,
        disablers,
    });
    // Bevy lifecycle dispatch requires component-scoped observer flags. A bare
    // global On<Add>/On<Remove> does not establish those archetype flags.
    world.spawn(
        Observer::new(
            |event: On<Add>,
             mut epoch: ResMut<StructureEpoch>,
             changes: Option<ResMut<Messages<SourceNodeChanged>>>| {
                epoch.version = epoch.version.wrapping_add(1);
                if let Some(mut changes) = changes {
                    changes.write(SourceNodeChanged {
                        entity: event.entity,
                        epoch: epoch.version,
                    });
                }
            },
        )
        .with_components(components.clone()),
    );
    world.spawn(
        Observer::new(
            |event: On<Remove>,
             mut epoch: ResMut<StructureEpoch>,
             changes: Option<ResMut<Messages<SourceNodeChanged>>>| {
                epoch.version = epoch.version.wrapping_add(1);
                if let Some(mut changes) = changes {
                    changes.write(SourceNodeChanged {
                        entity: event.entity,
                        epoch: epoch.version,
                    });
                }
            },
        )
        .with_components(components),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Component)]
    struct CustomDisabled;

    #[test]
    fn observes_arbitrary_component_membership_and_despawn() {
        let mut world = World::new();
        world.register_disabling_component::<CustomDisabled>();
        install(&mut world);
        let before = world.resource::<StructureEpoch>().version;
        let entity = world.spawn(Transform::IDENTITY).id();
        let added = world.resource::<StructureEpoch>().version;
        assert!(added > before);
        world.entity_mut(entity).insert(CustomDisabled);
        let disabled = world.resource::<StructureEpoch>().version;
        assert!(disabled > added);
        world.entity_mut(entity).remove::<CustomDisabled>();
        let enabled = world.resource::<StructureEpoch>().version;
        assert!(enabled > disabled);
        world.despawn(entity);
        assert!(world.resource::<StructureEpoch>().version > enabled);
    }
}
