//! Source primitive identity attached to glTF scene templates before instance cloning.

use bevy::{
    asset::LoadContext,
    gltf::{
        GltfLoaderSettings,
        extensions::{ErasedGltfExtensionHandler, GltfExtensionHandler, GltfExtensionHandlers},
        gltf,
    },
    prelude::*,
};
use std::collections::HashSet;

/// One occurrence in the source scene's depth-first primitive enumeration.
/// Mesh/primitive pairs alone are not unique when several nodes reuse a mesh.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct SourcePrimitive {
    pub scene_index: u32,
    pub ordinal: u32,
    pub mesh_index: u32,
    pub primitive_index: u32,
    pub matches_default_scene: bool,
}

/// Local transform captured on the completed, validated source scene template.
#[derive(Component, Clone, Copy, Debug, PartialEq, Reflect)]
#[reflect(Component)]
pub struct SourceNodePose(pub Transform);

/// Actual mesh handle captured from a tagged source primitive, before instancing.
#[derive(Component, Clone, Debug, PartialEq, Reflect)]
#[reflect(Component)]
pub struct SourcePrimitiveMesh(pub Handle<Mesh>);

/// Mesh-to-model matrix captured from the actual source hierarchy. No entity IDs
/// are stored: reflection can copy this baseline without relationship remapping.
#[derive(Component, Clone, Copy, Debug, PartialEq, Reflect)]
#[reflect(Component)]
pub struct SourcePrimitiveTransform(pub Mat4);

/// True when the raw document admits rigid resting geometry: no skins or morph targets.
/// Node animation is permitted; a runtime pose change revokes its resting instance.
/// This is a baseline fact, not a runtime permission to replace this entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct SourceStaticModel(pub bool);

/// Register after glTF's plugin and before requesting cluster-backed models.
pub struct SourceIdentityPlugin;

impl Plugin for SourceIdentityPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<SourcePrimitive>()
            .register_type::<SourceNodePose>()
            .register_type::<SourcePrimitiveMesh>()
            .register_type::<SourcePrimitiveTransform>()
            .register_type::<SourceStaticModel>();
        app.world_mut()
            .resource_mut::<GltfExtensionHandlers>()
            .0
            .write_blocking()
            .push(Box::<SourceIdentityHandler>::default());
    }
}

#[derive(Default)]
struct SourceIdentityHandler {
    default_scene: Option<usize>,
    static_model: bool,
    observed: Vec<(Entity, usize, usize)>,
}

// Bevy clones registered handlers for each asset load. Never share a load's
// pending entity IDs or enumeration state with another load.
impl Clone for SourceIdentityHandler {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl SourceIdentityHandler {
    fn begin_document(&mut self, source: &gltf::Gltf) {
        self.observed.clear();
        self.static_model = source.skins().next().is_none()
            && !source.meshes().any(|mesh| {
                mesh.primitives()
                    .any(|primitive| primitive.morph_targets().next().is_some())
            });
        self.default_scene = source
            .default_scene()
            .or_else(|| source.scenes().next())
            .map(|scene| scene.index());
    }

    fn complete_scene(&mut self, scene: &gltf::Scene<'_>, world: &mut World) -> bool {
        // Clear even on a refused scene, so the next scene cannot inherit it.
        let observed = std::mem::take(&mut self.observed);
        let expected = scene_primitives(scene);
        if observed.len() != expected.len()
            || !observed
                .iter()
                .zip(&expected)
                .all(|((_, mesh, primitive), pair)| (*mesh, *primitive) == *pair)
        {
            return false;
        }
        let Ok(scene_index) = u32::try_from(scene.index()) else {
            return false;
        };
        let mut seen = HashSet::new();
        let mut tags = Vec::with_capacity(observed.len());
        for (ordinal, (entity, mesh, primitive)) in observed.into_iter().enumerate() {
            let (Ok(ordinal), Ok(mesh_index), Ok(primitive_index)) = (
                u32::try_from(ordinal),
                u32::try_from(mesh),
                u32::try_from(primitive),
            ) else {
                return false;
            };
            if !seen.insert(entity) {
                return false;
            }
            let Some(mesh) = world.get::<Mesh3d>(entity) else {
                return false;
            };
            let Some(model_transform) = source_mesh_to_model(world, entity) else {
                return false;
            };
            tags.push((
                entity,
                SourcePrimitiveMesh(mesh.0.clone()),
                SourcePrimitiveTransform(model_transform),
                SourcePrimitive {
                    scene_index,
                    ordinal,
                    mesh_index,
                    primitive_index,
                    matches_default_scene: self.default_scene == Some(scene.index()),
                },
            ));
        }
        // Validate every observed primitive before attaching ANY tag or baseline.
        // The loader supplies this scene's isolated template World; capture the
        // actual local poses, including roots and primitive conversion transforms.
        let poses: Vec<_> = world
            .query::<(Entity, &Transform)>()
            .iter(world)
            .map(|(entity, pose)| (entity, SourceNodePose(*pose)))
            .collect();
        for (entity, pose) in poses {
            world.entity_mut(entity).insert(pose);
        }
        for (entity, mesh, transform, tag) in tags {
            world.entity_mut(entity).insert((
                tag,
                mesh,
                transform,
                SourceStaticModel(self.static_model),
            ));
        }
        true
    }
}

/// Accumulate actual local matrices, including the synthetic source scene root.
/// On an instantiated scene the external stock wrapper is not model-local input.
/// Missing entities or a repeated ancestor refuse the entire scene attachment.
fn source_mesh_to_model(world: &World, entity: Entity) -> Option<Mat4> {
    let mut current = entity;
    let mut accumulated = Mat4::IDENTITY;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current) {
            return None;
        }
        let entity = world.get_entity(current).ok()?;
        if entity.contains::<WorldAssetRoot>() {
            return Some(accumulated);
        }
        if let Some(local) = entity.get::<Transform>() {
            accumulated = local.to_matrix() * accumulated;
        }
        match entity.get::<ChildOf>() {
            Some(parent) => current = parent.parent(),
            None => return Some(accumulated),
        }
    }
}

fn scene_primitives(scene: &gltf::Scene<'_>) -> Vec<(usize, usize)> {
    let mut stack: Vec<_> = scene.nodes().collect();
    stack.reverse();
    let mut result = Vec::new();
    while let Some(node) = stack.pop() {
        if let Some(mesh) = node.mesh() {
            result.extend(
                mesh.primitives()
                    .map(|primitive| (mesh.index(), primitive.index())),
            );
        }
        let mut children: Vec<_> = node.children().collect();
        children.reverse();
        stack.extend(children);
    }
    result
}

impl GltfExtensionHandler for SourceIdentityHandler {
    fn dyn_clone(&self) -> Box<dyn ErasedGltfExtensionHandler> {
        Box::new(self.clone())
    }

    fn on_root(
        &mut self,
        _context: &mut LoadContext<'_>,
        source: &gltf::Gltf,
        _settings: &GltfLoaderSettings,
    ) {
        self.begin_document(source);
    }

    fn on_spawn_mesh_and_material(
        &mut self,
        _context: &mut LoadContext<'_>,
        primitive: &gltf::Primitive<'_>,
        mesh: &gltf::Mesh<'_>,
        _material: &gltf::Material<'_>,
        entity: &mut EntityWorldMut,
        _label: &str,
    ) {
        self.observed
            .push((entity.id(), mesh.index(), primitive.index()));
    }

    fn on_scene_completed(
        &mut self,
        context: &mut LoadContext<'_>,
        scene: &gltf::Scene<'_>,
        _root: Entity,
        world: &mut World,
    ) {
        if !self.complete_scene(scene, world) {
            warn!(asset = %context.path(), scene = scene.index(),
                "stock scene primitive enumeration differs from its source; cluster replacement unavailable");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(default_scene: usize) -> gltf::Gltf {
        let value = json!({
            "asset":{"version":"2.0"}, "scene":default_scene,
            "buffers":[{"byteLength":36,"uri":"unused.bin"}],
            "bufferViews":[{"buffer":0,"byteLength":36}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,
                "type":"VEC3","min":[0,0,0],"max":[1,1,1]}],
            "meshes":[
                {"name":"same","primitives":[{"attributes":{"POSITION":0}},{"attributes":{"POSITION":0}}]},
                {"name":"same","primitives":[{"attributes":{"POSITION":0}}]}
            ],
            "nodes":[
                {"name":"same","mesh":0,"children":[1]},
                {"name":"same","mesh":1},
                {"name":"same","mesh":0}
            ],
            "scenes":[{"nodes":[0,2]},{"nodes":[2]}]
        });
        gltf::Gltf::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn observe(
        handler: &mut SourceIdentityHandler,
        world: &mut World,
        pairs: &[(usize, usize)],
    ) -> Vec<Entity> {
        pairs
            .iter()
            .map(|&(mesh, primitive)| {
                let mesh_uuid =
                    bevy::asset::uuid::Uuid::from_u128(world.entities().len() as u128 + 1);
                let entity = world
                    .spawn(Mesh3d(Handle::Uuid(mesh_uuid, std::marker::PhantomData)))
                    .id();
                handler.observed.push((entity, mesh, primitive));
                entity
            })
            .collect()
    }

    #[test]
    fn repeated_meshes_equal_names_and_nested_nodes_keep_occurrence_ordinals() {
        let source = source(0);
        let scene = source.scenes().next().unwrap();
        let pairs = [(0, 0), (0, 1), (1, 0), (0, 0), (0, 1)];
        assert_eq!(scene_primitives(&scene), pairs);
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut world = World::new();
        let entities = observe(&mut handler, &mut world, &pairs);
        assert!(handler.complete_scene(&scene, &mut world));
        for (ordinal, entity) in entities.into_iter().enumerate() {
            let tag = world.get::<SourcePrimitive>(entity).unwrap();
            assert_eq!(tag.ordinal, ordinal as u32);
            assert_eq!(
                (tag.mesh_index, tag.primitive_index),
                (pairs[ordinal].0 as u32, pairs[ordinal].1 as u32)
            );
            assert!(tag.matches_default_scene);
        }
    }

    #[test]
    fn scenes_reset_ordinals_and_record_a_nonzero_default_scene() {
        let source = source(1);
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        for scene in source.scenes() {
            let mut world = World::new();
            let entities = observe(&mut handler, &mut world, &scene_primitives(&scene));
            assert!(handler.complete_scene(&scene, &mut world));
            assert!(handler.observed.is_empty());
            let tag = world.get::<SourcePrimitive>(entities[0]).unwrap();
            assert_eq!(tag.ordinal, 0);
            assert_eq!(tag.scene_index, scene.index() as u32);
            assert_eq!(tag.matches_default_scene, scene.index() == 1);
        }
    }

    #[test]
    fn mismatch_is_atomic_and_failed_scene_cannot_poison_the_next_scene() {
        let source = source(0);
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut world = World::new();
        let entities = observe(&mut handler, &mut world, &[(1, 0)]);
        assert!(!handler.complete_scene(&source.scenes().next().unwrap(), &mut world));
        assert!(world.get::<SourcePrimitive>(entities[0]).is_none());
        assert!(handler.observed.is_empty());
        let scene = source.scenes().nth(1).unwrap();
        observe(&mut handler, &mut world, &scene_primitives(&scene));
        assert!(handler.complete_scene(&scene, &mut world));
    }

    #[test]
    fn cloning_a_handler_starts_an_independent_empty_load() {
        let source = source(0);
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut world = World::new();
        observe(&mut handler, &mut world, &[(0, 0)]);
        let mut other = handler.clone();
        assert!(other.observed.is_empty());
        assert_eq!(other.default_scene, None);
        other.begin_document(&source);
        assert_eq!(handler.observed.len(), 1);
        assert!(other.observed.is_empty());
    }

    #[test]
    fn an_omitted_default_scene_selects_the_first_scene() {
        let mut value = serde_json::to_value(source(1).document.as_json()).unwrap();
        value.as_object_mut().unwrap().remove("scene");
        let source = gltf::Gltf::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(source.default_scene().is_none());
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        for scene in source.scenes() {
            let mut world = World::new();
            let entities = observe(&mut handler, &mut world, &scene_primitives(&scene));
            assert!(handler.complete_scene(&scene, &mut world));
            for entity in entities {
                assert_eq!(
                    world
                        .get::<SourcePrimitive>(entity)
                        .unwrap()
                        .matches_default_scene,
                    scene.index() == 0
                );
            }
        }
    }

    #[test]
    fn baselines_capture_actual_handles_and_all_local_poses_without_following_mutations() {
        let source = source(0);
        let scene = source.scenes().next().unwrap();
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut world = World::new();
        let parent_pose = Transform::from_xyz(2.0, 3.0, 4.0);
        let parent = world.spawn(parent_pose).id();
        let entities = observe(&mut handler, &mut world, &scene_primitives(&scene));
        let primitive = entities[0];
        let original_mesh = world.get::<Mesh3d>(primitive).unwrap().0.clone();
        let original_pose = Transform::from_xyz(5.0, 6.0, 7.0);
        world
            .entity_mut(primitive)
            .insert((original_pose, ChildOf(parent)));
        assert!(handler.complete_scene(&scene, &mut world));
        assert_eq!(
            world.get::<SourceNodePose>(parent),
            Some(&SourceNodePose(parent_pose))
        );
        assert_eq!(
            world.get::<SourceNodePose>(primitive),
            Some(&SourceNodePose(original_pose))
        );
        assert_eq!(
            world.get::<SourcePrimitiveMesh>(primitive).unwrap().0,
            original_mesh
        );
        assert_eq!(
            world.get::<SourceStaticModel>(primitive),
            Some(&SourceStaticModel(true))
        );
        world
            .entity_mut(primitive)
            .insert((Mesh3d(Handle::default()), Transform::IDENTITY));
        assert_eq!(
            world.get::<SourceNodePose>(primitive),
            Some(&SourceNodePose(original_pose))
        );
        assert_eq!(
            world.get::<SourcePrimitiveMesh>(primitive).unwrap().0,
            original_mesh
        );
        assert_ne!(world.get::<Mesh3d>(primitive).unwrap().0, original_mesh);
    }

    #[test]
    fn reparenting_unchanged_nodes_changes_the_captured_mesh_to_model_matrix() {
        let source = source(0);
        let scene = source.scenes().next().unwrap();
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut world = World::new();
        let root_pose =
            Transform::from_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        let root = world.spawn(root_pose).id();
        let first_pose = Transform::from_xyz(2.0, 0.0, 0.0);
        let second_pose = Transform::from_xyz(7.0, 0.0, 0.0);
        let first = world.spawn((first_pose, ChildOf(root))).id();
        let second = world.spawn((second_pose, ChildOf(root))).id();
        let entities = observe(&mut handler, &mut world, &scene_primitives(&scene));
        let primitive = entities[0];
        let local_pose = Transform::from_xyz(0.0, 3.0, 0.0);
        world
            .entity_mut(primitive)
            .insert((local_pose, ChildOf(first)));
        assert!(handler.complete_scene(&scene, &mut world));
        let captured = world.get::<SourcePrimitiveTransform>(primitive).unwrap().0;
        assert_eq!(
            captured,
            root_pose.to_matrix() * (first_pose.to_matrix() * local_pose.to_matrix())
        );
        world.entity_mut(primitive).insert(ChildOf(second));
        // Every local pose still matches its baseline, including the new parent.
        for entity in [root, first, second, primitive] {
            assert_eq!(
                world.get::<SourceNodePose>(entity).unwrap().0,
                *world.get::<Transform>(entity).unwrap()
            );
        }
        let current = source_mesh_to_model(&world, primitive).unwrap();
        assert_eq!(
            current,
            root_pose.to_matrix() * (second_pose.to_matrix() * local_pose.to_matrix())
        );
        assert_ne!(current, captured);
        assert_eq!(
            world.get::<SourcePrimitiveTransform>(primitive).unwrap().0,
            captured
        );
        assert!(source_mesh_to_model(&world, Entity::PLACEHOLDER).is_none());
    }

    #[test]
    fn a_repeated_ancestor_refuses_the_entire_scene_before_attachment() {
        let source = source(0);
        let scene = source.scenes().next().unwrap();
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut world = World::new();
        let first = world.spawn(Transform::IDENTITY).id();
        let second = world.spawn((Transform::IDENTITY, ChildOf(first))).id();
        // Relationship hooks maintain Children but do not reject multi-entity cycles.
        world.entity_mut(first).insert(ChildOf(second));
        let entities = observe(&mut handler, &mut world, &scene_primitives(&scene));
        world.entity_mut(entities[0]).insert(ChildOf(first));
        assert!(source_mesh_to_model(&world, entities[0]).is_none());
        assert!(!handler.complete_scene(&scene, &mut world));
        assert!(handler.observed.is_empty());
        for entity in entities.into_iter().chain([first, second]) {
            assert!(world.get::<SourcePrimitive>(entity).is_none());
            assert!(world.get::<SourcePrimitiveMesh>(entity).is_none());
            assert!(world.get::<SourcePrimitiveTransform>(entity).is_none());
            assert!(world.get::<SourceNodePose>(entity).is_none());
            assert!(world.get::<SourceStaticModel>(entity).is_none());
        }
        world.entity_mut(first).remove::<ChildOf>();
    }

    #[test]
    fn a_late_missing_mesh_refuses_all_tags_and_all_baselines() {
        let source = source(0);
        let scene = source.scenes().next().unwrap();
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut world = World::new();
        let parent = world.spawn(Transform::from_xyz(1.0, 2.0, 3.0)).id();
        let entities = observe(&mut handler, &mut world, &scene_primitives(&scene));
        world
            .entity_mut(*entities.last().unwrap())
            .remove::<Mesh3d>();
        assert!(!handler.complete_scene(&scene, &mut world));
        assert!(handler.observed.is_empty());
        for entity in entities.into_iter().chain([parent]) {
            assert!(world.get::<SourcePrimitive>(entity).is_none());
            assert!(world.get::<SourcePrimitiveMesh>(entity).is_none());
            assert!(world.get::<SourcePrimitiveTransform>(entity).is_none());
            assert!(world.get::<SourceNodePose>(entity).is_none());
            assert!(world.get::<SourceStaticModel>(entity).is_none());
        }
    }

    #[test]
    fn raw_node_animation_keeps_resting_geometry_but_skin_and_morph_do_not() {
        for variant in 0..3 {
            let mut value = serde_json::to_value(source(0).document.as_json()).unwrap();
            match variant {
                0 => {
                    value["accessors"].as_array_mut().unwrap().push(json!({
                        "bufferView":0,"componentType":5126,"count":3,
                        "type":"SCALAR","min":[0],"max":[1]
                    }));
                    value["animations"] = json!([{
                        "samplers":[{"input":1,"output":0}],
                        "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]
                    }]);
                }
                1 => value["skins"] = json!([{"joints":[0]}]),
                2 => value["meshes"][0]["primitives"][0]["targets"] = json!([{"POSITION":0}]),
                _ => unreachable!(),
            }
            let dynamic = gltf::Gltf::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
            let scene = dynamic.scenes().next().unwrap();
            let mut handler = SourceIdentityHandler::default();
            handler.begin_document(&dynamic);
            assert_eq!(handler.static_model, variant == 0, "variant {variant}");
            let mut world = World::new();
            let entities = observe(&mut handler, &mut world, &scene_primitives(&scene));
            assert!(handler.complete_scene(&scene, &mut world));
            for entity in entities {
                assert_eq!(
                    world.get::<SourceStaticModel>(entity),
                    Some(&SourceStaticModel(variant == 0))
                );
            }
            // Reusing a handler for a static document recomputes the trait.
            handler.begin_document(&source(0));
            assert!(handler.static_model);
            assert!(!handler.clone().static_model);
        }
    }

    #[test]
    fn reflection_preserves_source_tags_through_scene_clone_and_instance_write() {
        let mut app = App::new();
        app.init_resource::<GltfExtensionHandlers>()
            .add_plugins(SourceIdentityPlugin)
            .register_type::<Mesh3d>()
            .register_type::<Transform>()
            .register_type::<GlobalTransform>()
            .register_type::<bevy::transform::components::TransformTreeChanged>();
        let registry = app
            .world()
            .resource::<bevy::ecs::reflect::AppTypeRegistry>();
        let source = source(0);
        let scene = source.scenes().next().unwrap();
        let mut handler = SourceIdentityHandler::default();
        handler.begin_document(&source);
        let mut template = World::new();
        let entities = observe(&mut handler, &mut template, &scene_primitives(&scene));
        assert!(handler.complete_scene(&scene, &mut template));
        let expected: Vec<_> = entities
            .iter()
            .map(|&entity| *template.get::<SourcePrimitive>(entity).unwrap())
            .collect();
        let mut cloned = WorldAsset::new(template).clone_with(registry).unwrap();
        let copied: Vec<_> = cloned
            .world
            .query::<(Entity, &SourcePrimitive)>()
            .iter(&cloned.world)
            .map(|(entity, tag)| (entity, *tag))
            .collect();
        assert_eq!(copied.len(), expected.len());
        let mut actual: Vec<_> = copied.iter().map(|(_, tag)| *tag).collect();
        actual.sort_by_key(|tag| tag.ordinal);
        assert_eq!(actual, expected);

        let mut instance = World::new();
        // Occupy IDs so writing exercises the entity map, rather than relying on
        // the cloned scene's entity numbers being retained by the target world.
        for _ in 0..expected.len() + 1 {
            instance.spawn_empty();
        }
        let mut entity_map = bevy::ecs::entity::EntityHashMap::default();
        cloned
            .write_to_world_with(&mut instance, &mut entity_map, registry)
            .unwrap();
        for (source_entity, tag) in copied {
            let target = entity_map[&source_entity];
            assert_ne!(target, source_entity);
            assert_eq!(instance.get::<SourcePrimitive>(target), Some(&tag));
            assert_eq!(
                instance.get::<SourcePrimitiveTransform>(target),
                cloned.world.get::<SourcePrimitiveTransform>(source_entity),
            );
        }
    }
}

// Invented inputs, real asset loader and spawner.
#[cfg(test)]
pub(crate) mod actual_loader_tests {
    use super::*;
    use bevy::{
        asset::{AssetPlugin, LoadState, RecursiveDependencyLoadState},
        camera::{
            primitives::Aabb,
            visibility::{InheritedVisibility, ViewVisibility, VisibilityClass},
        },
        gltf::{Gltf, GltfMaterialName, GltfMeshName, GltfPlugin, GltfSceneName},
        mesh::MeshPlugin,
        transform::components::TransformTreeChanged,
        world_serialization::{WorldInstance, WorldInstanceSpawner, WorldSerializationPlugin},
    };
    use serde_json::json;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    fn invented_glb() -> Vec<u8> {
        fn floats(binary: &mut Vec<u8>, values: &[f32]) -> (usize, usize) {
            let offset = binary.len();
            for value in values {
                binary.extend(value.to_le_bytes());
            }
            (offset, binary.len() - offset)
        }
        let mut binary = Vec::new();
        let positions = floats(&mut binary, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        let raised_positions = floats(&mut binary, &[0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 1.0, 1.0]);
        let normals = floats(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let uvs = floats(&mut binary, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let indices_offset = binary.len();
        for index in [0u16, 1, 2] {
            binary.extend(index.to_le_bytes());
        }
        let views: Vec<_> = [
            positions,
            raised_positions,
            normals,
            uvs,
            (indices_offset, binary.len() - indices_offset),
        ]
        .into_iter()
        .map(|(offset, length)| json!({"buffer":0,"byteOffset":offset,"byteLength":length}))
        .collect();
        let primitive = json!({"attributes":{"POSITION":0,"NORMAL":2,"TEXCOORD_0":3},"indices":4});
        let raised = json!({"attributes":{"POSITION":1,"NORMAL":2,"TEXCOORD_0":3},"indices":4});
        let document = json!({
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":binary.len()}],
            "bufferViews":views,
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,1],"max":[1,1,1]},
                {"bufferView":2,"componentType":5126,"count":3,"type":"VEC3"},
                {"bufferView":3,"componentType":5126,"count":3,"type":"VEC2"},
                {"bufferView":4,"componentType":5123,"count":3,"type":"SCALAR"}
            ],
            "meshes":[
                {"name":"same","primitives":[primitive.clone(),raised]},
                {"name":"same","primitives":[primitive]}
            ],
            "nodes":[
                {"name":"same","mesh":0,"children":[1]},
                {"name":"same","mesh":1,"translation":[0,3,0]},
                {"name":"same","mesh":0,"translation":[2,0,0]}
            ],
            "scene":1,
            "scenes":[{"name":"same","nodes":[2]},{"name":"same","nodes":[0,2]}]
        });
        let mut json = serde_json::to_vec(&document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        while !binary.len().is_multiple_of(4) {
            binary.push(0);
        }
        let total = 12 + 8 + json.len() + 8 + binary.len();
        let mut bytes = Vec::with_capacity(total);
        bytes.extend(b"glTF");
        bytes.extend(2u32.to_le_bytes());
        bytes.extend((total as u32).to_le_bytes());
        bytes.extend((json.len() as u32).to_le_bytes());
        bytes.extend(b"JSON");
        bytes.extend(json);
        bytes.extend((binary.len() as u32).to_le_bytes());
        bytes.extend(b"BIN\0");
        bytes.extend(binary);
        bytes
    }

    pub(crate) fn loader_app(directory: &std::path::Path) -> App {
        loader_app_with(directory, |_| {})
    }

    pub(crate) fn loader_app_with(
        directory: &std::path::Path,
        configure: impl FnOnce(&mut App),
    ) -> App {
        let mut app = App::new();
        app.insert_resource(crate::config::EngineConfig {
            assets_dir: directory.to_path_buf(),
            ..default()
        })
        .init_resource::<crate::graphics_settings::GraphicsSettings>();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: directory.to_string_lossy().into_owned(),
                watch_for_changes_override: Some(false),
                ..default()
            },
            WorldSerializationPlugin,
            MeshPlugin,
            GltfPlugin::default(),
            SourceIdentityPlugin,
        ))
        .init_asset::<Image>()
        // The real GLB template contains these components; register them rather
        // than allowing a headless spawning failure to be treated as a skip.
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .register_type::<GlobalTransform>()
        .register_type::<TransformTreeChanged>()
        .register_type::<Visibility>()
        .register_type::<InheritedVisibility>()
        .register_type::<ViewVisibility>()
        .register_type::<VisibilityClass>()
        .register_type::<Mesh3d>()
        .register_type::<Aabb>()
        .register_type::<GltfMeshName>()
        .register_type::<GltfMaterialName>()
        .register_type::<GltfSceneName>();
        configure(&mut app);
        app.finish();
        app.cleanup();
        app
    }

    pub(crate) fn wait_for_loaded(app: &mut App, handle: &Handle<Gltf>) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            app.update();
            let server = app.world().resource::<AssetServer>();
            if let LoadState::Failed(error) = server.load_state(handle.id()) {
                panic!("invented source-identity GLB failed to load: {error}");
            }
            if let Some(RecursiveDependencyLoadState::Failed(error)) =
                server.get_recursive_dependency_load_state(handle.id())
            {
                panic!("invented source-identity GLB dependency failed: {error}");
            }
            if server.is_loaded_with_dependencies(handle.id()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "source-identity GLB timed out: root={:?}, dependencies={:?}",
                server.get_load_state(handle.id()),
                server.get_dependency_load_state(handle.id())
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    // Inspect actual hierarchy order, not tags sorted by their own ordinal:
    // sorting would conceal an ordinal attached to the wrong repeated node.
    fn hierarchy_tags(world: &World, root: Entity) -> Vec<SourcePrimitive> {
        let mut stack = vec![root];
        let mut tags = Vec::new();
        while let Some(entity) = stack.pop() {
            // WorldAssetRoot is an external wrapper, not part of the source scene.
            // Every source scene descendant with a Transform must retain its pose.
            if world.get::<WorldAssetRoot>(entity).is_none()
                && let Some(pose) = world.get::<Transform>(entity)
            {
                assert_eq!(
                    world.get::<SourceNodePose>(entity),
                    Some(&SourceNodePose(*pose))
                );
            }
            if let Some(tag) = world.get::<SourcePrimitive>(entity) {
                let mesh = world.get::<Mesh3d>(entity).expect("tagged mesh");
                assert_eq!(world.get::<SourcePrimitiveMesh>(entity).unwrap().0, mesh.0);
                assert_eq!(
                    world.get::<SourcePrimitiveTransform>(entity).unwrap().0,
                    source_mesh_to_model(world, entity).expect("acyclic actual instance hierarchy"),
                );
                assert_eq!(
                    world.get::<SourceStaticModel>(entity),
                    Some(&SourceStaticModel(true))
                );
                tags.push(*tag);
            }
            if let Some(children) = world.get::<Children>(entity) {
                stack.extend(children.into_iter().rev().copied());
            }
        }
        tags
    }

    fn occurrence_points(
        world: &World,
        root: Entity,
        meshes: &Assets<Mesh>,
    ) -> Vec<(u32, [f32; 3])> {
        let mut stack = vec![(root, Mat4::IDENTITY)];
        let mut points = Vec::new();
        while let Some((entity, parent_matrix)) = stack.pop() {
            let local = world
                .get::<Transform>(entity)
                .map_or(Mat4::IDENTITY, Transform::to_matrix);
            let model_matrix = parent_matrix * local;
            if let Some(tag) = world.get::<SourcePrimitive>(entity) {
                let handle = &world
                    .get::<Mesh3d>(entity)
                    .expect("tag belongs to a real mesh")
                    .0;
                let mesh = meshes
                    .get(handle)
                    .expect("tagged mesh asset is loaded in the main world");
                let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
                    mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                else {
                    panic!("fixture mesh has no Float32x3 POSITION attribute")
                };
                let point =
                    Vec3::from_array(*positions.first().expect("fixture triangle has vertices"));
                points.push((tag.ordinal, model_matrix.transform_point3(point).to_array()));
            }
            if let Some(children) = world.get::<Children>(entity) {
                stack.extend(
                    children
                        .into_iter()
                        .rev()
                        .map(|&child| (child, model_matrix)),
                );
            }
        }
        points.sort_by_key(|(ordinal, _)| *ordinal);
        points
    }

    fn assert_occurrence_points(actual: &[(u32, [f32; 3])], expected: &[[f32; 3]]) {
        let expected = expected
            .iter()
            .enumerate()
            .map(|(ordinal, &point)| (ordinal as u32, point))
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "source ordinal must identify the actual transformed primitive occurrence"
        );
    }

    fn assert_tags(tags: &[SourcePrimitive], scene: u32, expected: &[(u32, u32)]) {
        assert_eq!(tags.len(), expected.len());
        for (ordinal, (tag, &(mesh, primitive))) in tags.iter().zip(expected).enumerate() {
            assert_eq!(
                *tag,
                SourcePrimitive {
                    scene_index: scene,
                    ordinal: ordinal as u32,
                    mesh_index: mesh,
                    primitive_index: primitive,
                    matches_default_scene: scene == 1,
                }
            );
        }
    }

    #[test]
    fn actual_gltf_callbacks_match_converter_dfs_and_survive_world_asset_instancing() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = invented_glb();
        fs::write(directory.path().join("identity.glb"), &bytes).unwrap();
        let expected = [(0, 0), (0, 1), (1, 0), (0, 0), (0, 1)];
        let converted =
            converter::cluster::glb_input::load_model(&bytes, "meshes/identity.glb").unwrap();
        assert_eq!(converted.len(), expected.len());
        let expected_points = [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 3.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 0.0, 1.0],
        ];
        assert_eq!(
            converted
                .iter()
                .map(|primitive| primitive.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        // Geometry independently distinguishes primitive, child and repeated
        // node occurrences; merely asserting 0..count would miss a DFS reorder.
        assert_eq!(
            converted
                .iter()
                .map(|primitive| primitive.geometry.positions[0])
                .collect::<Vec<_>>(),
            expected_points
        );

        let mut app = loader_app(directory.path());
        let handle: Handle<Gltf> = app
            .world()
            .resource::<AssetServer>()
            .load_builder()
            .with_settings(|settings: &mut GltfLoaderSettings| {
                settings.load_animations = false;
                settings.load_cameras = false;
                settings.load_lights = false;
            })
            .load("identity.glb");
        wait_for_loaded(&mut app, &handle);
        let scenes = {
            let models = app.world().resource::<Assets<Gltf>>();
            let model = models.get(&handle).expect("loaded GLB root asset");
            assert_eq!(model.scenes.len(), 2);
            assert_eq!(model.default_scene.as_ref(), Some(&model.scenes[1]));
            model.scenes.clone()
        };
        for (scene, handle) in scenes.iter().enumerate() {
            let (tags, points) =
                app.world_mut()
                    .resource_scope(|world, mut assets: Mut<Assets<WorldAsset>>| {
                        let mut template =
                            assets.get_mut(handle).expect("loaded GLB scene template");
                        // WorldAsset also owns serialization bookkeeping entities.
                        // Select the root emitted by the GLTF loader, rather than
                        // assuming every parentless entity belongs to the scene.
                        let mut roots = template
                            .world
                            .query_filtered::<Entity, With<GltfSceneName>>();
                        let roots = roots.iter(&template.world).collect::<Vec<_>>();
                        assert_eq!(roots.len(), 1, "one Bevy-generated scene root");
                        assert!(template.world.get::<ChildOf>(roots[0]).is_none());
                        (
                            hierarchy_tags(&template.world, roots[0]),
                            occurrence_points(
                                &template.world,
                                roots[0],
                                world.resource::<Assets<Mesh>>(),
                            ),
                        )
                    });
            assert_tags(
                &tags,
                scene as u32,
                if scene == 0 {
                    &expected[..2]
                } else {
                    &expected
                },
            );
            assert_occurrence_points(
                &points,
                if scene == 0 {
                    &expected_points[3..]
                } else {
                    &expected_points
                },
            );
        }

        let root = app
            .world_mut()
            .spawn(WorldAssetRoot(scenes[1].clone()))
            .id();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            app.update();
            let world = app.world();
            let ready = world.get::<WorldInstance>(root).is_some_and(|instance| {
                world
                    .resource::<WorldInstanceSpawner>()
                    .instance_is_ready(**instance)
            });
            if ready {
                assert_tags(&hierarchy_tags(world, root), 1, &expected);
                assert_occurrence_points(
                    &occurrence_points(world, root, world.resource::<Assets<Mesh>>()),
                    &expected_points,
                );
                // These are real copied descendants of the wrapper, not only
                // tags remaining on the asset's source/template world.
                assert!(world.get::<Children>(root).is_some());
                app.world_mut()
                    .entity_mut(root)
                    .insert(Transform::from_xyz(100.0, 200.0, 300.0));
                // Wrapper placement is instance-to-world, not mesh-to-model.
                assert_tags(&hierarchy_tags(app.world(), root), 1, &expected);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "source-identity instance timed out: attached={}, children={:?}",
                world.get::<WorldInstance>(root).is_some(),
                world.get::<Children>(root)
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
