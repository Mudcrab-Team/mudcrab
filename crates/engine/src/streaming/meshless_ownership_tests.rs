mod meshless_ownership {
    use super::*;
    use crate::cluster::ownership::{OwnershipFacts, OwnershipReference};
    use crate::cluster::reader::CertifiedPackage;
    use bevy::asset::{AssetApp, AssetPlugin};
    use std::sync::Arc;

    struct Fixture {
        app: App,
        _directory: tempfile::TempDir,
        package: Arc<CertifiedPackage>,
        source: Handle<WorldAsset>,
        source_node: Entity,
        reference_root: Entity,
        extras: String,
    }

    fn fixture(with_collider: bool) -> Fixture {
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let reference = package.records().next().unwrap().1.reference;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<WorldAsset>()
            .insert_resource(EngineConfig::default())
            .init_resource::<StreamingWorld>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, arm_pending_models);
        crate::cluster::ownership::install(&mut app);

        // Stop at the real ownership-to-spawner handoff. Collider is not Reflect in
        // Rapier 0.36, and this proof must not substitute an unrelated spawn failure.
        // Both loaded-source shapes contain gameplay data and no Mesh3d.
        let extras = serde_json::json!({
            "mudcrabCollision": { "fixture": "meshless-gameplay-source" },
            "ownershipRegression": "preserve-before-and-after-certificate"
        })
        .to_string();
        let mut source_world = World::new();
        let source_node = source_world
            .spawn((
                Name::new("meshless gameplay source"),
                Transform::default(),
                GltfSceneExtras {
                    value: extras.clone(),
                },
            ))
            .id();
        if with_collider {
            source_world
                .entity_mut(source_node)
                .insert(Collider::cuboid(1.0, 2.0, 3.0));
        }
        let source = app
            .world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .add(WorldAsset::new(source_world));
        let reference_root = app
            .world_mut()
            .spawn((
                PendingModel {
                    handle: source.clone(),
                    sequence: 0,
                },
                OwnershipReference(reference),
            ))
            .id();
        assert!(
            app.world()
                .resource::<Assets<WorldAsset>>()
                .contains(source.id()),
            "the actual model gate sees a loaded source, not a load failure"
        );
        assert!(
            !app.world()
                .resource::<OwnershipFacts>()
                .can_spawn(reference),
            "the certificate is genuinely undecided before arming"
        );
        Fixture {
            app,
            _directory: directory,
            package,
            source,
            source_node,
            reference_root,
            extras,
        }
    }

    fn assert_raw_gameplay_source(fixture: &Fixture, with_collider: bool) {
        let world = fixture.app.world();
        let binding = world
            .get::<WorldAssetRoot>(fixture.reference_root)
            .expect("the real pending-model driver must arm the loaded meshless source");
        assert_eq!(
            binding.0.id(),
            fixture.source.id(),
            "meshless gameplay data must bind the raw source before any certificate"
        );
        assert!(world.get::<PendingModel>(fixture.reference_root).is_none());
        assert!(
            world
                .get::<crate::cluster::flat_source::OriginalStockScene>(fixture.reference_root)
                .is_none(),
            "no empty/filtered substitute may replace a meshless source"
        );
        let assets = world.resource::<Assets<WorldAsset>>();
        assert_eq!(
            assets.len(),
            1,
            "arming must not allocate an empty substitute"
        );
        let bound = assets.get(&binding.0).unwrap();
        assert_eq!(
            bound
                .world
                .iter_entities()
                .filter(|node| node.contains::<GltfSceneExtras>())
                .count(),
            1,
            "exactly one gameplay source node remains, excluding singleton resource entities"
        );
        assert_eq!(
            bound
                .world
                .get::<GltfSceneExtras>(fixture.source_node)
                .unwrap()
                .value,
            fixture.extras,
            "the scene handed to the spawner retains its gameplay extras"
        );
        assert_eq!(
            bound.world.get::<Collider>(fixture.source_node).is_some(),
            with_collider,
            "collider-only source data must survive the ownership handoff"
        );
        if with_collider {
            let half_extents = bound
                .world
                .get::<Collider>(fixture.source_node)
                .unwrap()
                .as_cuboid()
                .unwrap()
                .half_extents();
            assert_eq!(half_extents, Vec3::new(1.0, 2.0, 3.0));
        }
        assert!(
            !world.contains_resource::<crate::streaming::failure::LoadingRefusal>(),
            "ordinary meshless source data is not an ownership refusal"
        );
        assert!(world.get::<AssetFailed>(fixture.reference_root).is_none());
    }

    #[test]
    fn pending_meshless_gameplay_source_arms_raw_before_certificate() {
        for with_collider in [false, true] {
            let mut fixture = fixture(with_collider);
            fixture.app.update();
            assert_raw_gameplay_source(&fixture, with_collider);
        }
    }

    #[test]
    fn certificate_publication_cannot_erase_an_already_armed_meshless_source() {
        for with_collider in [false, true] {
            let mut fixture = fixture(with_collider);
            fixture.app.update();
            // Do not assert the initial binding here: this independent test reaches
            // publication on the unfixed candidate and catches permanent data loss.
            fixture
                .app
                .world_mut()
                .resource_mut::<OwnershipFacts>()
                .publish(
                    &fixture.package,
                    fixture
                        .package
                        .records()
                        .map(|(key, _)| (*key, false))
                        .collect(),
                );
            for _ in 0..3 {
                fixture.app.update();
            }
            assert_raw_gameplay_source(&fixture, with_collider);
        }
    }
    #[derive(Component, Clone, Reflect)]
    #[reflect(Component)]
    struct CustomHiddenMesh;

    fn hidden_mesh_fixture(custom: bool) -> Fixture {
        use crate::cluster::source_identity::SourcePrimitive;
        use bevy::ecs::entity_disabling::{DefaultQueryFilters, Disabled};
        let mut fixture = fixture(false);
        let ordinal = fixture.package.records().next().unwrap().1.primitive;
        fixture
            .app
            .register_type::<Name>()
            .register_type::<Transform>()
            .register_type::<GltfSceneExtras>()
            .register_type::<Mesh3d>()
            .register_type::<SourcePrimitive>()
            .register_type::<Disabled>()
            .register_type::<CustomHiddenMesh>();
        {
            let mut assets = fixture.app.world_mut().resource_mut::<Assets<WorldAsset>>();
            let mut source = assets.get_mut(&fixture.source).unwrap();
            source.world.entity_mut(fixture.source_node).insert((
                Mesh3d(Handle::default()),
                SourcePrimitive {
                    scene_index: 0,
                    ordinal,
                    mesh_index: 0,
                    primitive_index: 0,
                    matches_default_scene: true,
                },
            ));
            if custom {
                let component = source.world.register_component::<CustomHiddenMesh>();
                source
                    .world
                    .resource_mut::<DefaultQueryFilters>()
                    .register_disabling_component(component);
                source
                    .world
                    .entity_mut(fixture.source_node)
                    .insert(CustomHiddenMesh);
            } else {
                source
                    .world
                    .entity_mut(fixture.source_node)
                    .insert(Disabled);
            }
            assert!(
                source
                    .world
                    .iter_entities()
                    .any(|node| node.contains::<Mesh3d>()),
                "the loaded template really contains a render body, including hidden entities"
            );
            assert!(
                !source
                    .world
                    .try_query::<&Mesh3d>()
                    .is_some_and(|mut meshes| meshes.iter(&source.world).next().is_some()),
                "the native/custom default filter must hide that body from an ordinary query"
            );
        }
        fixture
    }

    fn assert_hidden_mesh_is_held(fixture: &Fixture) {
        let world = fixture.app.world();
        assert!(
            world.get::<PendingModel>(fixture.reference_root).is_some(),
            "a disabled render body still awaits its ownership certificate"
        );
        assert!(
            world
                .get::<WorldAssetRoot>(fixture.reference_root)
                .is_none(),
            "an undecided hidden render body must never bind a raw scene"
        );
        assert!(world.get::<AssetFailed>(fixture.reference_root).is_none());
        assert!(!world.contains_resource::<crate::streaming::failure::LoadingRefusal>());
    }

    fn qualify_hidden_mesh_and_assert_filtered(mut fixture: Fixture) {
        // Reach the before->after transition without an initial assertion so this
        // independent test exposes the decided-owner raw fallback on the unfixed pin.
        fixture.app.update();
        fixture
            .app
            .world_mut()
            .resource_mut::<OwnershipFacts>()
            .publish(
                &fixture.package,
                fixture
                    .package
                    .records()
                    .map(|(key, _)| (*key, false))
                    .collect(),
            );
        for _ in 0..3 {
            fixture.app.update();
        }
        let world = fixture.app.world();
        let binding = world
            .get::<WorldAssetRoot>(fixture.reference_root)
            .expect("a qualified owned source must finish arming");
        assert_ne!(
            binding.0.id(),
            fixture.source.id(),
            "owned render bodies need a filtered template even while disabled"
        );
        assert!(world.get::<PendingModel>(fixture.reference_root).is_none());
        assert_eq!(
            world
                .get::<crate::cluster::flat_source::OriginalStockScene>(fixture.reference_root)
                .expect("filtered ownership retains original source identity")
                .0
                .id(),
            fixture.source.id()
        );
        let assets = world.resource::<Assets<WorldAsset>>();
        let filtered = assets.get(&binding.0).unwrap();
        assert!(
            !filtered
                .world
                .iter_entities()
                .any(|node| node.contains::<Mesh3d>()),
            "inspect all entities: default filters cannot conceal a raw owned Mesh3d"
        );
        let retained = filtered
            .world
            .iter_entities()
            .find(|node| node.contains::<crate::cluster::ownership::OwnedPrimitiveSource>())
            .expect("filtering keeps the hidden primitive's nonrender source data");
        assert_eq!(
            retained.get::<GltfSceneExtras>().unwrap().value,
            fixture.extras
        );
        assert!(
            assets
                .get(&fixture.source)
                .unwrap()
                .world
                .get::<Mesh3d>(fixture.source_node)
                .is_some(),
            "the original template remains complete and unmodified"
        );
        assert!(world.get::<AssetFailed>(fixture.reference_root).is_none());
        assert!(
            !world.contains_resource::<crate::streaming::failure::LoadingRefusal>(),
            "an empty fail-closed refusal must not masquerade as successful owned filtering"
        );
    }

    #[test]
    fn native_disabled_mesh_waits_for_certificate_before_arming() {
        let mut fixture = hidden_mesh_fixture(false);
        fixture.app.update();
        assert_hidden_mesh_is_held(&fixture);
    }

    #[test]
    fn custom_disabled_mesh_waits_for_certificate_before_arming() {
        let mut fixture = hidden_mesh_fixture(true);
        fixture.app.update();
        assert_hidden_mesh_is_held(&fixture);
    }

    #[test]
    fn native_disabled_owned_mesh_is_filtered_after_qualification() {
        qualify_hidden_mesh_and_assert_filtered(hidden_mesh_fixture(false));
    }

    #[test]
    fn custom_disabled_owned_mesh_is_filtered_after_qualification() {
        qualify_hidden_mesh_and_assert_filtered(hidden_mesh_fixture(true));
    }
}
