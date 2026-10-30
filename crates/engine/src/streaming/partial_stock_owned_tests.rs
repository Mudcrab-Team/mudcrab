// Included by partial_stock.rs's tests module.
mod owned_body_regressions {
    use super::*;
    use crate::{
        cluster::{
            ownership::{OwnershipFacts, OwnershipReference},
            source_identity::{SourcePrimitive, SourcePrimitiveMesh, SourcePrimitiveTransform},
        },
        streaming::{
            StaticColliderSource,
            tests::{add_converted_model, model_app, settle_readiness, spawn_pending_reference},
        },
    };
    use std::collections::BTreeMap;

    #[test]
    fn loaded_owned_scene_has_no_render_body_and_keeps_full_bounds_and_proxy_collision() {
        let (_directory, package, row) = fixture();
        let mut app = model_app();
        app.add_plugins(bevy::transform::TransformPlugin)
            .register_type::<bevy::transform::components::TransformTreeChanged>()
            .register_type::<SourcePrimitive>()
            .register_type::<SourcePrimitiveMesh>()
            .register_type::<SourcePrimitiveTransform>();
        crate::cluster::ownership::install(&mut app);
        app.world_mut().resource_mut::<OwnershipFacts>().publish(
            &package,
            package
                .records()
                .map(|(key, _)| (*key, false))
                .collect::<BTreeMap<_, _>>(),
        );
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2.0, 4.0, 6.0));
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let owned: BTreeSet<_> = package
            .records()
            .filter_map(|(_, identity)| {
                (identity.reference == row.form_id).then_some(identity.primitive)
            })
            .collect();
        let refused = package
            .placed_reference(row.form_id)
            .unwrap()
            .stock_primitives
            .clone()
            .unwrap();
        let mut scene = World::new();
        let support = scene
            .spawn((Name::new("gameplay support"), Transform::IDENTITY))
            .id();
        for &ordinal in owned.union(&refused) {
            let pose =
                Transform::from_xyz(if owned.contains(&ordinal) { 10.0 } else { 0.0 }, 0.0, 0.0);
            scene.spawn((
                Name::new("gameplay primitive"),
                pose,
                ChildOf(support),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                SourcePrimitiveMesh(mesh.clone()),
                SourcePrimitiveTransform(pose.to_matrix()),
                SourcePrimitive {
                    scene_index: 0,
                    ordinal,
                    mesh_index: 0,
                    primitive_index: ordinal,
                    matches_default_scene: true,
                },
            ));
        }
        let source = add_converted_model(&mut app, scene);
        let root = spawn_pending_reference(&mut app, source.clone(), 0);
        app.world_mut().entity_mut(root).insert((
            OwnershipReference(row.form_id),
            ExpectedModelBounds::new(Vec3::new(-1.0, -2.0, -3.0), Vec3::new(11.0, 2.0, 3.0))
                .unwrap(),
        ));
        {
            let mut pending = app
                .world_mut()
                .get_mut::<PendingAssetProfile>(root)
                .unwrap();
            pending.static_physics = true;
            pending.base_record_type.clone_from(&row.base_record_type);
            pending.path = "meshes/architecture/owned-regression.glb".into();
        }
        // Assert in the first frame after WorldAssetRoot spawning, before readiness can strip bodies.
        app.update();
        let mut checked_owned = 0;
        for node in app.world().iter_entities() {
            if let Some(identity) = node.get::<SourcePrimitive>()
                && owned.contains(&identity.ordinal)
            {
                checked_owned += 1;
                assert!(
                    !node.contains::<Mesh3d>(),
                    "owned body instantiated before readiness"
                );
                assert!(!node.contains::<MeshMaterial3d<StandardMaterial>>());
                assert_eq!(node.get::<SourcePrimitiveMesh>().unwrap().0, mesh);
                assert_eq!(node.get::<Name>().unwrap().as_str(), "gameplay primitive");
                assert!(node.contains::<ChildOf>());
            }
        }
        assert_eq!(
            checked_owned,
            owned.len(),
            "owned gameplay nodes did not instantiate"
        );
        assert_eq!(
            app.world_mut().query::<&Mesh3d>().iter(app.world()).count(),
            refused.len(),
            "first instantiation permits only fixed refused source bodies"
        );
        let metrics = settle_readiness(&mut app);
        assert_eq!(metrics.transform_bounds_validation_failures, 0);
        assert_eq!(metrics.asset_load_failures, 0);
        assert_eq!(metrics.assets_ready, 1);
        assert_eq!(
            app.world().get::<StaticColliderSource>(root),
            Some(&StaticColliderSource::RenderProxy)
        );
        let collider = app
            .world()
            .get::<bevy_rapier3d::prelude::Collider>(root)
            .unwrap()
            .as_trimesh()
            .unwrap();
        assert!(
            collider.vertices().any(|v| v.x >= 11.0),
            "owned geometry lost from collider"
        );
        assert_eq!(
            app.world()
                .resource::<Assets<WorldAsset>>()
                .get(&source)
                .unwrap()
                .world
                .iter_entities()
                .filter(|e| e.contains::<Mesh3d>())
                .count(),
            owned.len() + refused.len()
        );
        // Let native initial Added/Modified delivery quiesce before requesting a hot reload.
        // The native spawner debounces those initial publications; this test exercises a
        // subsequent edit through its actual event consumer, without changing that policy.
        let mut events = app
            .world()
            .resource::<Messages<AssetEvent<WorldAsset>>>()
            .get_cursor_current();
        let mut quiet_updates = 0;
        for _ in 0..16 {
            app.update();
            if events
                .read(app.world().resource::<Messages<AssetEvent<WorldAsset>>>())
                .next()
                .is_none()
            {
                quiet_updates += 1;
            } else {
                quiet_updates = 0;
            }
            if quiet_updates == 4 {
                break;
            }
        }
        assert_eq!(
            quiet_updates, 4,
            "initial world-asset publication did not quiesce"
        );
        // Reloading the source or withdrawing its material qualification cannot
        // restore an owned body. The refreshed template still carries gameplay data.
        app.world_mut().resource_mut::<OwnershipFacts>().publish(
            &package,
            package.records().map(|(key, _)| (*key, true)).collect(),
        );
        {
            let mut assets = app.world_mut().resource_mut::<Assets<WorldAsset>>();
            let mut scene = assets.get_mut(&source).unwrap();
            let entities: Vec<_> = scene
                .world
                .iter_entities()
                .filter(|node| {
                    node.get::<SourcePrimitive>()
                        .is_some_and(|tag| owned.contains(&tag.ordinal))
                })
                .map(|node| node.id())
                .collect();
            for entity in entities {
                scene
                    .world
                    .entity_mut(entity)
                    .insert(Name::new("edited gameplay primitive"));
            }
            // This tagged primitive was never permitted by the certificate. Its
            // bounds fit inside the existing aggregate, so bounds cannot reject it.
            scene.world.spawn((
                Name::new("added gameplay primitive"),
                Transform::IDENTITY,
                ChildOf(support),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                SourcePrimitiveMesh(mesh.clone()),
                SourcePrimitiveTransform(Mat4::IDENTITY),
                SourcePrimitive {
                    scene_index: 0,
                    ordinal: u32::MAX,
                    mesh_index: 0,
                    primitive_index: u32::MAX,
                    matches_default_scene: true,
                },
            ));
        }
        for _ in 0..8 {
            app.update();
        }
        // Separate template refresh from the native spawner's event delivery.
        let filtered = &app.world().get::<WorldAssetRoot>(root).unwrap().0;
        let assets = app.world().resource::<Assets<WorldAsset>>();
        let cached = &assets.get(filtered).unwrap().world;
        let refreshed = cached
            .iter_entities()
            .filter(|node| {
                node.get::<SourcePrimitive>()
                    .is_some_and(|tag| owned.contains(&tag.ordinal))
            })
            .filter(|node| {
                node.get::<Name>()
                    .is_some_and(|name| name.as_str() == "edited gameplay primitive")
            })
            .count();
        assert_eq!(
            refreshed,
            owned.len(),
            "the source event must update its filtered gameplay template"
        );
        let mut edited = 0;
        for node in app.world().iter_entities() {
            if node
                .get::<SourcePrimitive>()
                .is_some_and(|tag| owned.contains(&tag.ordinal))
            {
                edited += 1;
                assert!(!node.contains::<Mesh3d>());
                assert!(!node.contains::<MeshMaterial3d<StandardMaterial>>());
                assert_eq!(
                    node.get::<Name>().unwrap().as_str(),
                    "edited gameplay primitive"
                );
            }
        }
        assert_eq!(edited, owned.len());
        let added: Vec<_> = app
            .world()
            .iter_entities()
            .filter(|node| {
                node.get::<SourcePrimitive>()
                    .is_some_and(|tag| tag.ordinal == u32::MAX)
            })
            .collect();
        assert_eq!(
            added.len(),
            1,
            "edited gameplay occurrence did not instantiate"
        );
        assert_eq!(
            added[0].get::<Name>().unwrap().as_str(),
            "added gameplay primitive"
        );
        assert!(
            !added[0].contains::<Mesh3d>(),
            "new source ordinal received stock permission during reload"
        );
        assert!(!added[0].contains::<MeshMaterial3d<StandardMaterial>>());
        assert_eq!(added[0].get::<SourcePrimitiveMesh>().unwrap().0, mesh);
        drop(added);
        assert_eq!(
            app.world_mut().query::<&Mesh3d>().iter(app.world()).count(),
            refused.len(),
            "hot reload permits only fixed refused source bodies"
        );
    }
}
