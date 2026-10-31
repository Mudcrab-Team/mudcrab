//! Distant stock draw templates with exact source primitive identity.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use bevy::{
    ecs::{reflect::AppTypeRegistry, resource::IsResource},
    prelude::*,
    transform::components::TransformTreeChanged,
};

use crate::cluster::source_identity::{
    SourceNodePose, SourcePrimitive, SourcePrimitiveMesh, SourcePrimitiveTransform,
    SourceStaticModel,
};

/// Clone the selected default-scene draws, retaining their original local hierarchy.
///
/// Keeping the ancestor chain avoids decomposing an accumulated matrix back into
/// a `Transform`, which cannot represent shear. Selected render entities retain
/// their cloned components; other retained entities carry only structural state.
/// The caller retains the ordinary scene for skin/morph dependency graphs.
pub(super) fn selected_scene(
    template: &WorldAsset,
    ordinals: &BTreeSet<u32>,
    registry: &AppTypeRegistry,
) -> Result<WorldAsset, String> {
    let mut scene = template
        .clone_with(registry)
        .map_err(|error| format!("cannot clone partial stock template: {error}"))?;
    let mut primitives = BTreeMap::new();
    let mut scene_index = None;
    for entity in scene.world.iter_entities() {
        if !entity.contains::<Mesh3d>() {
            continue;
        }
        let tag = entity.get::<SourcePrimitive>().ok_or_else(|| {
            format!(
                "stock mesh {:?} is missing SourcePrimitive identity",
                entity.id()
            )
        })?;
        if !tag.matches_default_scene {
            return Err(format!(
                "stock primitive {} is not in the default scene",
                tag.ordinal
            ));
        }
        if scene_index.is_some_and(|index| index != tag.scene_index) {
            return Err("stock primitive identities disagree on the default scene".to_owned());
        }
        scene_index = Some(tag.scene_index);
        if primitives.insert(tag.ordinal, entity.id()).is_some() {
            return Err(format!("duplicate stock primitive ordinal {}", tag.ordinal));
        }
    }

    let mut selected = HashSet::new();
    for ordinal in ordinals {
        let entity = primitives
            .get(ordinal)
            .ok_or_else(|| format!("unknown selected stock primitive ordinal {ordinal}"))?;
        selected.insert(*entity);
    }
    let mut keep = HashSet::new();
    for &entity in &selected {
        let mut current = entity;
        let mut path = HashSet::new();
        loop {
            if !path.insert(current) {
                return Err(format!("cycle in stock primitive ancestry at {current:?}"));
            }
            if !keep.insert(current) {
                break;
            }
            let ancestor = scene.world.get_entity(current).map_err(|error| {
                format!("invalid stock primitive ancestor {current:?}: {error}")
            })?;
            match ancestor.get::<ChildOf>() {
                Some(parent) => current = parent.parent(),
                None => break,
            }
        }
    }

    let remove: Vec<_> = scene
        .world
        .iter_entities()
        .filter(|entity| !entity.contains::<IsResource>() && !keep.contains(&entity.id()))
        .map(|entity| entity.id())
        .collect();
    for entity in remove {
        // Removing a branch also despawns its descendants via Children. A later
        // entry may therefore already be gone; do not emit a spurious warning.
        if let Ok(entity) = scene.world.get_entity_mut(entity) {
            entity.despawn();
        }
    }
    for entity in keep.difference(&selected) {
        // An unselected Mesh3d can itself be a necessary ancestor. Retain an
        // allowlist so it loses every draw/material/behavior component, including
        // components unknown here, without changing its pose or relationships.
        scene.world.entity_mut(*entity).retain::<(
            Name,
            Transform,
            GlobalTransform,
            TransformTreeChanged,
            ChildOf,
            Children,
            Visibility,
            InheritedVisibility,
            ViewVisibility,
            SourceNodePose,
            SourcePrimitive,
            SourcePrimitiveMesh,
            SourcePrimitiveTransform,
            SourceStaticModel,
            (
                bevy::animation::AnimationTargetId,
                bevy::animation::AnimatedBy,
                AnimationPlayer,
                bevy::animation::graph::AnimationGraphHandle,
                crate::model_animation::SourceClips,
                bevy::gltf::GltfExtras,
            ),
        )>();
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        camera::{
            primitives::CubemapFrusta,
            visibility::{CubemapVisibleEntities, RenderLayers, VisibilityClass},
        },
        light::{NotShadowCaster, NotShadowReceiver},
        transform::{TransformPlugin, components::TransformTreeChanged},
    };

    #[derive(Component, Clone, Debug, PartialEq, Reflect)]
    #[reflect(Component)]
    struct RenderFlag(u32);

    #[derive(Component, Clone, Reflect)]
    #[reflect(Component)]
    struct NonDrawBehavior;

    struct Fixture {
        template: WorldAsset,
        registry: AppTypeRegistry,
        owned: Entity,
        refused: Entity,
        mesh: Handle<Mesh>,
        material: Handle<StandardMaterial>,
    }

    fn fixture() -> Fixture {
        let mut app = App::new();
        app.register_type::<ChildOf>()
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
            .register_type::<MeshMaterial3d<StandardMaterial>>()
            .register_type::<SourcePrimitive>()
            .register_type::<SourceNodePose>()
            .register_type::<SourcePrimitiveMesh>()
            .register_type::<SourcePrimitiveTransform>()
            .register_type::<SourceStaticModel>()
            .register_type::<RenderLayers>()
            .register_type::<NotShadowCaster>()
            .register_type::<NotShadowReceiver>()
            .register_type::<RenderFlag>()
            .register_type::<NonDrawBehavior>()
            .register_type::<PointLight>()
            .register_type::<CubemapFrusta>()
            .register_type::<CubemapVisibleEntities>();
        let registry = app.world().resource::<AppTypeRegistry>().clone();
        let mesh = Handle::<Mesh>::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let owned_material = materials.add(StandardMaterial::default());
        let material = materials.add(StandardMaterial {
            alpha_mode: AlphaMode::Mask(0.37),
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        let mut world = World::new();
        // Nonuniform scale followed by a rotated child produces real shear.
        let root = world
            .spawn((
                Name::new("root"),
                Transform::from_xyz(3.0, -2.0, 7.0).with_scale(Vec3::new(2.0, 3.0, 0.5)),
                Visibility::Hidden,
                NonDrawBehavior,
                PointLight::default(),
            ))
            .id();
        let pivot = world
            .spawn((
                Name::new("pivot"),
                Transform::from_rotation(Quat::from_rotation_z(0.61)),
                Visibility::Inherited,
                ChildOf(root),
                NonDrawBehavior,
            ))
            .id();
        let mut spawn_primitive = |ordinal, name, parent, material| {
            world
                .spawn((
                    Name::new(name),
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material),
                    Transform::from_xyz(ordinal as f32 + 1.0, 2.0, -4.0)
                        .with_rotation(Quat::from_rotation_x(0.29)),
                    Visibility::Inherited,
                    SourcePrimitive {
                        scene_index: 2,
                        ordinal,
                        mesh_index: 7,
                        primitive_index: 0,
                        matches_default_scene: true,
                    },
                    SourcePrimitiveMesh(mesh.clone()),
                    SourceStaticModel(true),
                    ChildOf(parent),
                ))
                .id()
        };
        let owned = spawn_primitive(0, "owned", pivot, owned_material);
        let refused = spawn_primitive(1, "refused", pivot, material.clone());
        world.entity_mut(refused).insert((
            NotShadowCaster,
            NotShadowReceiver,
            RenderLayers::layer(3),
            RenderFlag(91),
        ));
        let unused = world
            .spawn((Name::new("unused"), Transform::IDENTITY, ChildOf(root)))
            .id();
        world.spawn((
            Name::new("unused-light"),
            PointLight::default(),
            ChildOf(unused),
        ));
        let poses: Vec<_> = world
            .query::<(Entity, &Transform)>()
            .iter(&world)
            .map(|(entity, pose)| (entity, *pose))
            .collect();
        for (entity, pose) in poses {
            world.entity_mut(entity).insert(SourceNodePose(pose));
        }
        for entity in [owned, refused] {
            let model = model_matrix(&world, entity);
            world
                .entity_mut(entity)
                .insert(SourcePrimitiveTransform(model));
        }
        Fixture {
            template: WorldAsset::new(world),
            registry,
            owned,
            refused,
            mesh,
            material,
        }
    }

    fn named(world: &World, name: &str) -> Entity {
        world
            .iter_entities()
            .find(|entity| {
                entity
                    .get::<Name>()
                    .is_some_and(|value| value.as_str() == name)
            })
            .unwrap_or_else(|| panic!("missing {name}"))
            .id()
    }

    fn model_matrix(world: &World, mut entity: Entity) -> Mat4 {
        let mut matrix = Mat4::IDENTITY;
        loop {
            if let Some(pose) = world.get::<Transform>(entity) {
                matrix = pose.to_matrix() * matrix;
            }
            match world.get::<ChildOf>(entity) {
                Some(parent) => entity = parent.parent(),
                None => return matrix,
            }
        }
    }

    fn placed_matrix(template: &WorldAsset, registry: &AppTypeRegistry) -> Mat4 {
        let mut app = App::new();
        app.add_plugins(TransformPlugin);
        let placement = app
            .world_mut()
            .spawn(
                Transform::from_xyz(30.0, 20.0, -10.0).with_rotation(Quat::from_rotation_y(0.43)),
            )
            .id();
        let mut map = bevy::ecs::entity::EntityHashMap::default();
        template
            .write_to_world_with(app.world_mut(), &mut map, registry)
            .unwrap();
        for &entity in map.values() {
            if app.world().get::<ChildOf>(entity).is_none() {
                app.world_mut()
                    .entity_mut(entity)
                    .insert(ChildOf(placement));
            }
        }
        app.update();
        let entity = named(app.world(), "refused");
        app.world()
            .get::<GlobalTransform>(entity)
            .unwrap()
            .to_matrix()
    }

    #[test]
    fn selects_refused_ordinal_one_without_drawing_owned_repeated_mesh_zero() {
        let input = fixture();
        let selected =
            selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).unwrap();
        let draws: Vec<_> = selected
            .world
            .iter_entities()
            .filter(|entity| entity.contains::<Mesh3d>())
            .collect();
        assert_eq!(draws.len(), 1);
        let draw = &draws[0];
        assert_eq!(draw.get::<SourcePrimitive>().unwrap().ordinal, 1);
        assert_eq!(draw.get::<Mesh3d>().unwrap().0, input.mesh);
        assert_eq!(
            draw.get::<MeshMaterial3d<StandardMaterial>>().unwrap().0,
            input.material
        );
        assert!(draw.contains::<NotShadowCaster>());
        assert!(draw.contains::<NotShadowReceiver>());
        assert_eq!(draw.get::<RenderLayers>(), Some(&RenderLayers::layer(3)));
        assert_eq!(draw.get::<RenderFlag>(), Some(&RenderFlag(91)));
        assert_eq!(draw.get::<Visibility>(), Some(&Visibility::Inherited));
        // The source template remains a full scene after selection.
        assert!(input.template.world.get::<Mesh3d>(input.owned).is_some());
    }

    #[test]
    fn preserves_sheared_model_and_placed_world_matrices_without_flattening() {
        let input = fixture();
        let expected = model_matrix(&input.template.world, input.refused);
        let selected =
            selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).unwrap();
        let refused = named(&selected.world, "refused");
        // This fixture would expose a lossy matrix -> Transform decomposition.
        let (scale, rotation, translation) = expected.to_scale_rotation_translation();
        assert!(
            !Mat4::from_scale_rotation_translation(scale, rotation, translation)
                .abs_diff_eq(expected, 1e-4)
        );
        assert_eq!(model_matrix(&selected.world, refused), expected);
        assert_eq!(
            selected.world.get::<SourcePrimitiveTransform>(refused),
            input
                .template
                .world
                .get::<SourcePrimitiveTransform>(input.refused)
        );
        assert_eq!(
            placed_matrix(&selected, &input.registry),
            placed_matrix(&input.template, &input.registry)
        );
    }

    #[test]
    fn keeps_only_required_ancestry_and_strips_ancestor_behavior_and_lights() {
        let input = fixture();
        let selected =
            selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).unwrap();
        let mut names: Vec<_> = selected
            .world
            .iter_entities()
            .filter_map(|entity| entity.get::<Name>().map(|name| name.as_str().to_owned()))
            .collect();
        names.sort();
        assert_eq!(names, ["pivot", "refused", "root"]);
        for entity in selected.world.iter_entities() {
            assert!(!entity.contains::<NonDrawBehavior>());
            assert!(!entity.contains::<PointLight>());
        }
        let root = named(&selected.world, "root");
        let pivot = named(&selected.world, "pivot");
        let refused = named(&selected.world, "refused");
        assert_eq!(
            selected.world.get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        assert_eq!(
            selected.world.get::<Children>(root).unwrap().as_ref(),
            &[pivot]
        );
        assert_eq!(
            selected.world.get::<Children>(pivot).unwrap().as_ref(),
            &[refused]
        );
        assert_eq!(
            selected.world.get::<ChildOf>(refused).unwrap().parent(),
            pivot
        );
    }

    #[test]
    fn preserves_render_animation_and_billboard_metadata_on_required_ancestors() {
        let mut input = fixture();
        {
            let mut registry = input.registry.write();
            registry.register::<AnimationPlayer>();
            registry.register::<bevy::gltf::GltfExtras>();
            registry.register::<crate::model_animation::SourceClips>();
        }
        let pivot = named(&input.template.world, "pivot");
        input.template.world.entity_mut(pivot).insert((
            AnimationPlayer::default(),
            bevy::gltf::GltfExtras {
                value: r#"{"openSkyrim":{"billboard":"alwaysFaceCamera"}}"#.into(),
            },
        ));
        let root = named(&input.template.world, "root");
        input
            .template
            .world
            .entity_mut(root)
            .insert(crate::model_animation::SourceClips::default());
        let selected =
            selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).unwrap();
        let pivot = named(&selected.world, "pivot");
        assert!(selected.world.get::<AnimationPlayer>(pivot).is_some());
        assert!(
            selected
                .world
                .get::<bevy::gltf::GltfExtras>(pivot)
                .is_some()
        );
        assert!(
            selected
                .world
                .get::<crate::model_animation::SourceClips>(named(&selected.world, "root"))
                .is_some()
        );
    }

    #[test]
    fn keeps_unselected_primitive_as_pose_only_when_selected_child_needs_it() {
        let mut input = fixture();
        input
            .template
            .world
            .entity_mut(input.refused)
            .insert(ChildOf(input.owned));
        input.template.world.entity_mut(input.owned).insert((
            NotShadowCaster,
            RenderFlag(17),
            NonDrawBehavior,
        ));
        let expected = model_matrix(&input.template.world, input.refused);
        let selected =
            selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).unwrap();
        let ancestor = selected.world.entity(named(&selected.world, "owned"));
        assert!(!ancestor.contains::<Mesh3d>());
        assert!(!ancestor.contains::<MeshMaterial3d<StandardMaterial>>());
        assert!(!ancestor.contains::<NotShadowCaster>());
        assert!(!ancestor.contains::<RenderFlag>());
        assert!(!ancestor.contains::<NonDrawBehavior>());
        assert_eq!(
            ancestor.get::<Transform>(),
            input.template.world.get::<Transform>(input.owned)
        );
        assert_eq!(
            model_matrix(&selected.world, named(&selected.world, "refused")),
            expected
        );
    }

    #[test]
    fn retains_both_selected_ancestor_and_descendant_draws() {
        let mut input = fixture();
        input
            .template
            .world
            .entity_mut(input.refused)
            .insert(ChildOf(input.owned));
        let selected =
            selected_scene(&input.template, &BTreeSet::from([0, 1]), &input.registry).unwrap();
        let mut ordinals: Vec<_> = selected
            .world
            .iter_entities()
            .filter(|entity| entity.contains::<Mesh3d>())
            .map(|entity| entity.get::<SourcePrimitive>().unwrap().ordinal)
            .collect();
        ordinals.sort();
        assert_eq!(ordinals, [0, 1]);
    }

    #[test]
    fn rejects_unknown_selected_ordinal_instead_of_returning_partial_success() {
        let input = fixture();
        assert!(selected_scene(&input.template, &BTreeSet::from([1, 9]), &input.registry).is_err());
    }

    #[test]
    fn rejects_missing_identity_even_on_an_unselected_mesh() {
        let mut input = fixture();
        input
            .template
            .world
            .entity_mut(input.owned)
            .remove::<SourcePrimitive>();
        assert!(selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).is_err());
    }

    #[test]
    fn rejects_duplicate_primitive_ordinal() {
        let mut input = fixture();
        input
            .template
            .world
            .get_mut::<SourcePrimitive>(input.owned)
            .unwrap()
            .ordinal = 1;
        assert!(selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).is_err());
    }

    #[test]
    fn rejects_nondefault_primitive_identity_even_when_unselected() {
        let mut input = fixture();
        input
            .template
            .world
            .get_mut::<SourcePrimitive>(input.owned)
            .unwrap()
            .matches_default_scene = false;
        assert!(selected_scene(&input.template, &BTreeSet::from([1]), &input.registry).is_err());
    }

    #[test]
    fn empty_selection_keeps_no_scene_entities() {
        let input = fixture();
        let selected = selected_scene(&input.template, &BTreeSet::new(), &input.registry).unwrap();
        assert!(
            selected
                .world
                .iter_entities()
                .all(|entity| entity.contains::<bevy::ecs::resource::IsResource>())
        );
    }

    #[test]
    fn returns_reflection_clone_errors() {
        let input = fixture();
        assert!(
            selected_scene(
                &input.template,
                &BTreeSet::from([1]),
                &AppTypeRegistry::default(),
            )
            .is_err()
        );
    }
}
