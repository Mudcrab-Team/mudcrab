// Included by partial_stock.rs's existing tests module.
mod partial_stock_readiness {
    use super::*;
    use crate::nif_material::NifNativeMaterialSource;
    use crate::{
        cluster::{
            flat_source::{FlatSourceOwner, FlatSourcePrimitive, FlatSourcePrimitives},
            source_identity::{
                SourceNodePose, SourcePrimitive, SourcePrimitiveMesh, SourcePrimitiveTransform,
                SourceStaticModel,
            },
        },
        nif_depth::{DECAL, DEPTH_TEST, NifDepthMaterial, NifDepthState, depth_material},
        streaming::{
            PendingAssetProfile, PendingModel, converted_model_path, matrix_max_difference,
            tests::{add_converted_model, model_app, settle_readiness, spawn_pending_reference},
            track_asset_readiness,
        },
        world::components::{CellRef, FormId, WorldTransform},
    };
    use bevy::{
        asset::AssetApp,
        camera::visibility::VisibilityClass,
        transform::{TransformPlugin, components::TransformTreeChanged},
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum DrawMaterial {
        Standard(Handle<StandardMaterial>),
        NifDepth {
            rendered: Handle<NifDepthMaterial>,
            source: Handle<StandardMaterial>,
        },
    }

    fn draw_material(
        standard: Option<&MeshMaterial3d<StandardMaterial>>,
        depth: Option<&MeshMaterial3d<NifDepthMaterial>>,
        source: Option<&NifNativeMaterialSource>,
    ) -> DrawMaterial {
        match (standard, depth, source) {
            (Some(standard), None, None) => DrawMaterial::Standard(standard.0.clone()),
            (None, Some(depth), Some(source)) => DrawMaterial::NifDepth {
                rendered: depth.0.clone(),
                source: source.0.clone(),
            },
            _ => panic!("a draw must retain exactly one native material path"),
        }
    }

    #[derive(Resource, Default)]
    struct BeforeFlatten(BTreeMap<Entity, (SourcePrimitive, DrawMaterial, GlobalTransform)>);

    // Observe only validated draws while hierarchy still exists. Native material
    // handles are present at spawn, before readiness proves their source and bounds.
    #[allow(clippy::type_complexity)]
    fn observe_validated_draws(
        mut observed: ResMut<BeforeFlatten>,
        draws: Query<
            (
                Entity,
                &SourcePrimitive,
                Option<&MeshMaterial3d<StandardMaterial>>,
                Option<&MeshMaterial3d<NifDepthMaterial>>,
                Option<&NifNativeMaterialSource>,
                &GlobalTransform,
                Has<Transform>,
                Has<ChildOf>,
            ),
            Without<FlatSourceOwner>,
        >,
        parents: Query<&ChildOf>,
        ready_roots: Query<
            (),
            (
                With<FormId>,
                Without<PendingAssetProfile>,
                Without<AssetFailed>,
            ),
        >,
    ) {
        for (entity, identity, standard, depth, source, global, local, parent) in &draws {
            if !parents
                .iter_ancestors(entity)
                .any(|root| ready_roots.contains(root))
            {
                continue;
            }
            assert!(
                local && parent,
                "ordinary readiness must precede flattening"
            );
            observed.0.entry(entity).or_insert((
                *identity,
                draw_material(standard, depth, source),
                *global,
            ));
        }
    }

    #[test]
    fn partial_readiness_preserves_standard_material_handles_and_source_affine() {
        assert_partial_readiness_preserves_source_draws(false);
    }

    #[test]
    fn partial_readiness_preserves_nif_depth_material_handles_and_source_affine() {
        assert_partial_readiness_preserves_source_draws(true);
    }

    fn assert_partial_readiness_preserves_source_draws(nif_depth: bool) {
        let (_directory, package, row) = fixture();
        let reference = row.form_id;
        let placed = package.placed_reference(reference).unwrap();
        let refused = placed.stock_primitives.clone().unwrap();
        let accepted: BTreeSet<_> = package
            .records()
            .filter_map(|(_, identity)| {
                (identity.reference == reference).then_some(identity.primitive)
            })
            .collect();
        assert!(!accepted.is_empty() && !refused.is_empty());
        assert!(accepted.is_disjoint(&refused));
        let ordinals: BTreeSet<_> = accepted.union(&refused).copied().collect();
        let path = converted_model_path(row.model_path.clone().unwrap()).unwrap();

        let mut app = model_app();
        app.add_plugins(TransformPlugin)
            .init_asset::<NifDepthMaterial>()
            .register_type::<MeshMaterial3d<NifDepthMaterial>>()
            .register_type::<NifNativeMaterialSource>()
            .init_resource::<BeforeFlatten>()
            .register_type::<TransformTreeChanged>()
            .register_type::<Visibility>()
            .register_type::<InheritedVisibility>()
            .register_type::<ViewVisibility>()
            .register_type::<VisibilityClass>()
            .register_type::<SourceNodePose>()
            .register_type::<SourcePrimitive>()
            .register_type::<SourcePrimitiveMesh>()
            .register_type::<SourcePrimitiveTransform>()
            .register_type::<SourceStaticModel>()
            .add_systems(Update, observe_validated_draws.after(track_asset_readiness));
        install(&mut app);
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2.0, 4.0, 6.0));
        let base_color = Color::srgb(0.4, 0.3, 0.2);
        let base = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color,
                alpha_mode: AlphaMode::Mask(0.37),
                double_sided: true,
                cull_mode: None,
                ..default()
            });
        let depth_state =
            NifDepthState::from_flags(DEPTH_TEST | DECAL, 0, AlphaMode::Mask(0.37)).unwrap();
        let expected_material = if nif_depth {
            let material = app
                .world()
                .resource::<Assets<StandardMaterial>>()
                .get(&base)
                .unwrap()
                .clone();
            let rendered = app
                .world_mut()
                .resource_mut::<Assets<NifDepthMaterial>>()
                .add(depth_material(material, depth_state));
            DrawMaterial::NifDepth {
                rendered,
                source: base.clone(),
            }
        } else {
            DrawMaterial::Standard(base.clone())
        };
        let parent_pose = Transform::from_scale(Vec3::new(2.0, 1.0, 0.5));
        let primitive_pose = Transform::from_xyz(0.25, 0.5, -0.75)
            .with_rotation(Quat::from_rotation_z(0.61) * Quat::from_rotation_x(0.29));
        let model_matrix = parent_pose.to_matrix() * primitive_pose.to_matrix();
        assert!(
            model_matrix
                .x_axis
                .truncate()
                .dot(model_matrix.y_axis.truncate())
                .abs()
                > 0.1,
            "the fixture must contain shear that a decomposed Transform cannot retain"
        );
        let identities: BTreeMap<_, _> = ordinals
            .iter()
            .map(|&ordinal| {
                (
                    ordinal,
                    SourcePrimitive {
                        scene_index: 0,
                        ordinal,
                        mesh_index: 0,
                        primitive_index: ordinal,
                        matches_default_scene: true,
                    },
                )
            })
            .collect();
        let mut scene = World::new();
        let parent = scene.spawn((parent_pose, SourceNodePose(parent_pose))).id();
        for identity in identities.values() {
            let mut node = scene.spawn((
                primitive_pose,
                SourceNodePose(primitive_pose),
                Mesh3d(mesh.clone()),
                *identity,
                SourcePrimitiveMesh(mesh.clone()),
                SourcePrimitiveTransform(model_matrix),
                SourceStaticModel(true),
                ChildOf(parent),
            ));
            match &expected_material {
                DrawMaterial::Standard(material) => {
                    node.insert(MeshMaterial3d(material.clone()));
                }
                DrawMaterial::NifDepth { rendered, source } => {
                    node.insert((
                        MeshMaterial3d(rendered.clone()),
                        NifNativeMaterialSource(source.clone()),
                    ));
                }
            }
        }
        // Independently transform all eight cuboid corners. These are full-model
        // bounds, not a refused subset's replacement for the original contract.
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for x in [-1.0, 1.0] {
            for y in [-2.0, 2.0] {
                for z in [-3.0, 3.0] {
                    let corner = model_matrix.transform_point3(Vec3::new(x, y, z));
                    min = min.min(corner);
                    max = max.max(corner);
                }
            }
        }
        let bounds = ExpectedModelBounds::new(min, max).unwrap();
        let source = add_converted_model(&mut app, scene);
        let full = spawn_pending_reference(&mut app, source.clone(), 0);
        let partial = spawn_pending_reference(&mut app, source.clone(), 1);
        let placement = Transform::from_xyz(17.0, 23.0, -9.0)
            .with_rotation(Quat::from_rotation_y(0.4))
            .with_scale(Vec3::splat(1.75));
        for root in [full, partial] {
            app.world_mut().entity_mut(root).insert((
                placement,
                GlobalTransform::from(placement),
                WorldTransform(placement.to_matrix()),
                Visibility::Inherited,
                FormId(row.form_id),
                CellRef(row.cell_id),
                bounds,
            ));
            let mut entity = app.world_mut().entity_mut(root);
            let mut pending = entity.get_mut::<PendingAssetProfile>().unwrap();
            pending.path.clone_from(&path);
            pending.form_id = row.form_id;
            pending.base_form_id = row.base_form_id;
            pending.cell_id = row.cell_id;
            pending.base_record_type.clone_from(&row.base_record_type);
            pending.static_physics = false;
        }
        app.world_mut()
            .entity_mut(partial)
            .insert(PartialStock::candidate(&package, &row).unwrap());
        assert!(app.world().get::<PendingAssetProfile>(partial).is_some());
        assert!(app.world().get::<WorldAssetRoot>(partial).is_none());
        let metrics = settle_readiness(&mut app);
        assert_eq!(metrics.assets_ready, 2);
        assert_eq!(metrics.bounds_validated, 2);
        assert_eq!(
            metrics.meshes_validated,
            (ordinals.len() + refused.len()) as u64
        );
        assert_eq!(metrics.asset_load_failures, 0);
        assert_eq!(metrics.material_validation_failures, 0);
        assert_eq!(metrics.transform_bounds_validation_failures, 0);
        assert_eq!(
            app.world_mut()
                .query::<&AssetFailed>()
                .iter(app.world())
                .count(),
            0
        );
        for root in [full, partial] {
            assert!(app.world().get::<PendingAssetProfile>(root).is_none());
            assert!(app.world().get::<PendingModel>(root).is_none());
        }
        assert_eq!(
            app.world().get::<OriginalStockScene>(partial).unwrap().0,
            source
        );
        let full_draws: BTreeMap<_, _> = app
            .world_mut()
            .query_filtered::<(
                &SourcePrimitive,
                Option<&MeshMaterial3d<StandardMaterial>>,
                Option<&MeshMaterial3d<NifDepthMaterial>>,
                Option<&NifNativeMaterialSource>,
                &GlobalTransform,
            ), Without<FlatSourceOwner>>()
            .iter(app.world())
            .map(|(identity, standard, depth, source, global)| {
                (
                    identity.ordinal,
                    (*identity, draw_material(standard, depth, source), *global),
                )
            })
            .collect();
        assert_eq!(full_draws.len(), ordinals.len());
        let flattened: Vec<_> = app
            .world()
            .get::<FlatSourcePrimitives>(partial)
            .unwrap()
            .iter()
            .collect();
        assert_eq!(flattened.len(), refused.len());
        for entity in &flattened {
            let draw = app.world().entity(*entity);
            let identity = *draw.get::<SourcePrimitive>().unwrap();
            let material = draw_material(
                draw.get::<MeshMaterial3d<StandardMaterial>>(),
                draw.get::<MeshMaterial3d<NifDepthMaterial>>(),
                draw.get::<NifNativeMaterialSource>(),
            );
            let global = *draw.get::<GlobalTransform>().unwrap();
            assert_eq!(identity, identities[&identity.ordinal]);
            assert!(refused.contains(&identity.ordinal));
            assert_eq!(material, expected_material);
            assert_eq!(
                (identity, material.clone(), global),
                full_draws[&identity.ordinal]
            );
            assert_eq!(
                &(identity, material.clone(), global),
                &app.world().resource::<BeforeFlatten>().0[entity],
                "flattening must retain the same validated source draw"
            );
            assert!(draw.get::<Transform>().is_none());
            assert!(draw.get::<ChildOf>().is_none());
            assert_eq!(draw.get::<FlatSourceOwner>().unwrap().0, partial);
            assert_eq!(
                draw.get::<SourcePrimitiveTransform>().unwrap().0,
                model_matrix
            );
            let proof = draw.get::<FlatSourcePrimitive>().unwrap();
            assert_eq!(proof.model_matrix, model_matrix);
            assert_eq!(proof.expected_global, global);
            let expected = placement.to_matrix() * model_matrix;
            assert!(matrix_max_difference(global.to_matrix(), expected) < 1.0e-4);
            let materials = app.world().resource::<Assets<StandardMaterial>>();
            let source_material = materials.get(&base).unwrap();
            assert_eq!(source_material.base_color, base_color);
            assert_eq!(source_material.alpha_mode, AlphaMode::Mask(0.37));
            assert!(source_material.double_sided);
            assert_eq!(source_material.cull_mode, None);
            if let DrawMaterial::NifDepth { rendered, source } = &material {
                assert_eq!(*source, base);
                let depth_materials = app.world().resource::<Assets<NifDepthMaterial>>();
                let rendered = depth_materials.get(rendered).unwrap();
                assert_eq!(rendered.base.base_color, base_color);
                assert_eq!(rendered.base.alpha_mode, source_material.alpha_mode);
                assert_eq!(rendered.base.double_sided, source_material.double_sided);
                assert_eq!(rendered.base.cull_mode, source_material.cull_mode);
                assert_eq!(rendered.extension.state, depth_state);
            }
        }
        assert_eq!(
            app.world().resource::<BeforeFlatten>().0.len(),
            ordinals.len() + refused.len()
        );

        // Refused bodies keep their exact source material and affine while
        // owned bodies were never instantiated in the distant scene.
        for _ in 0..8 {
            app.update();
        }
        let remaining: BTreeSet<_> = app
            .world()
            .get::<FlatSourcePrimitives>(partial)
            .unwrap()
            .iter()
            .map(|entity| app.world().get::<SourcePrimitive>(entity).unwrap().ordinal)
            .collect();
        assert_eq!(remaining, refused);
        for entity in flattened {
            assert!(app.world().get_entity(entity).is_ok());
            assert!(app.world().get::<Transform>(entity).is_none());
            assert!(app.world().get::<ChildOf>(entity).is_none());
        }
        {
            let mut assets = app.world_mut().resource_mut::<Assets<WorldAsset>>();
            let mut original = assets.get_mut(&source).unwrap();
            let nodes: Vec<_> = original
                .world
                .iter_entities()
                .filter(|node| node.contains::<Mesh3d>())
                .map(|node| node.id())
                .collect();
            for node in nodes {
                original
                    .world
                    .entity_mut(node)
                    .insert(Name::new("edited source primitive"));
            }
        }
        for _ in 0..12 {
            app.update();
        }
        assert!(app.world().get::<PartialStock>(partial).is_some());
        assert!(app.world().get::<PendingAssetProfile>(partial).is_none());
        let refreshed: Vec<_> = app
            .world()
            .get::<FlatSourcePrimitives>(partial)
            .unwrap()
            .iter()
            .collect();
        assert_eq!(refreshed.len(), refused.len());
        for entity in refreshed {
            let node = app.world().entity(entity);
            let identity = node.get::<SourcePrimitive>().unwrap();
            assert!(refused.contains(&identity.ordinal));
            assert_eq!(
                node.get::<Name>().unwrap().as_str(),
                "edited source primitive"
            );
            assert_eq!(
                draw_material(
                    node.get::<MeshMaterial3d<StandardMaterial>>(),
                    node.get::<MeshMaterial3d<NifDepthMaterial>>(),
                    node.get::<NifNativeMaterialSource>(),
                ),
                full_draws[&identity.ordinal].1
            );
            assert_eq!(
                *node.get::<GlobalTransform>().unwrap(),
                full_draws[&identity.ordinal].2
            );
            assert!(node.get::<Transform>().is_none());
        }
        assert!(app.world().get::<AssetFailed>(partial).is_none());
    }
}
