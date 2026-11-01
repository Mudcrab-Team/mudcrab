// Included inside readiness_bridge.rs's existing test module after the baseline is frozen.

#[test]
fn batch_facts_are_shared_across_consumers_without_sharing_view_readiness() {
    let mut producer = Producer::default();
    let facts = std::cell::Cell::new(0);
    let worlds = producer
        .observe(
            catalog_key(),
            [0; 9],
            vec![
                consumer(1, false, Some(Arc::new(vec![true]))),
                consumer(2, true, Some(Arc::new(vec![false]))),
            ],
            needed,
            |batch| {
                assert_eq!(batch, 0);
                facts.set(facts.get() + 1);
                (true, false, false, true)
            },
        )
        .unwrap();
    assert_eq!(
        worlds[&42],
        ClusterWorldReadiness {
            applicable_cameras: 1,
            needed_batches: 2,
            pending_templates: 2,
            pending_pipelines: 1,
            ..default()
        }
    );
    assert_eq!(facts.get(), 1, "batch facts do not depend on the consumer");
}

#[test]
fn gpu_identity_changes_refresh_batch_failure_and_recovery_without_rebuilding_inventory() {
    let mut producer = Producer::default();
    let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
    let mut key = catalog_key();
    let catalog_calls = std::cell::Cell::new(0);
    let catalog = || {
        catalog_calls.set(catalog_calls.get() + 1);
        needed()
    };
    let initial = producer
        .observe(key, [0; 9], consumers.clone(), catalog, |batch| {
            assert_eq!(batch, 0);
            (true, true, false, true)
        })
        .expect("the initial observation must publish readiness");
    let ready = ClusterWorldReadiness {
        applicable_cameras: 1,
        needed_batches: 1,
        ..default()
    };
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[&42], ready);
    assert_eq!(catalog_calls.get(), 1);
    catalog_calls.set(0);

    // GPU replacement changes current drawing proof without changing placed inventory.
    // Keep the revision and consumer Arc identities fixed to isolate this input.
    key.gpu_identity += 1;
    let failed = producer
        .observe(key, [0; 9], consumers.clone(), catalog, |batch| {
            assert_eq!(batch, 0);
            (false, false, true, true)
        })
        .expect("a new GPU identity must not replay the previous ready aggregate");
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[&42],
        ClusterWorldReadiness {
            applicable_cameras: 1,
            needed_batches: 1,
            pending_compute: 1,
            pending_templates: 1,
            failed_batches: 1,
            ..default()
        }
    );
    assert!(
        producer
            .observe(
                key,
                [0; 9],
                consumers.clone(),
                || panic!("an unchanged observation must reuse inventory"),
                |_| panic!("an unchanged observation must reuse the failed aggregate"),
            )
            .is_none()
    );

    key.gpu_identity += 1;
    let recovered = producer
        .observe(key, [0; 9], consumers.clone(), catalog, |batch| {
            assert_eq!(batch, 0);
            (true, true, false, true)
        })
        .expect("another GPU identity must replace the previous failed aggregate");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[&42], ready);
    assert!(
        producer
            .observe(
                key,
                [0; 9],
                consumers,
                || panic!("an unchanged observation must reuse inventory"),
                |_| panic!("an unchanged observation must reuse the recovered aggregate"),
            )
            .is_none()
    );
    assert_eq!(
        catalog_calls.get(),
        0,
        "GPU identity changes must refresh batch readiness without reconstructing inventory"
    );
}

fn install_empty_inventory_scene(
    main: &mut MainWorld,
    package: &Arc<Certificate>,
    layout: u64,
) -> Arc<Inventory> {
    let inventory = Arc::new(Inventory::build(package.clone(), layout, &[], 0).unwrap());
    main.insert_resource(ClusterScene {
        ready_revision: default(),
        residency_revision: default(),
        certificate: Some(package.clone()),
        inventory: Some(inventory.clone()),
        terrain_blocks: Vec::new(),
        meshes: Vec::new(),
        instances: Vec::new(),
        origin_offset: Vec3::ZERO,
        blocks: Vec::new(),
    });
    inventory
}

#[test]
fn publication_rejects_foreign_inventory_arc_and_same_arc_recovery_recomputes_proof() {
    use bevy::ecs::system::RunSystemOnce;
    let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
    let package =
        Arc::new(crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap());
    let lazy = crate::cluster::package::LazyBatches::unrequested_for_test(1, Vec3::ZERO);
    let layout = lazy.layout_scope;
    let mut main = MainWorld::default();
    main.insert_resource(crate::cluster::package::ValidatedPackage(package.clone()));
    main.insert_resource(lazy);
    let inventory = install_empty_inventory_scene(&mut main, &package, layout);
    main.insert_resource(crate::config::EngineConfig {
        worldspace_id: 42,
        ..default()
    });
    main.init_resource::<crate::streaming::ActiveSpace>();
    main.init_resource::<ClusterLoadingGate>();
    let generation = main.resource::<ClusterLoadingGate>().generation();
    let mut render = World::new();
    render.insert_resource(main);
    render.init_resource::<Producer>();
    let consumers = vec![consumer(1, false, Some(Arc::new(vec![true])))];
    let worlds = render
        .resource_mut::<Producer>()
        .observe(catalog_key(), [0; 9], consumers.clone(), needed, |_| {
            (true, true, false, true)
        })
        .unwrap();
    render.resource_mut::<Producer>().pending = Some(Snapshot {
        package: package.clone(),
        inventory: inventory.clone(),
        layout,
        generation,
        frame: 1,
        worlds,
    });
    // Equal contents, package, layout, generation and frame do not substitute for
    // the main scene's retained inventory allocation.
    render
        .resource_mut::<MainWorld>()
        .resource_mut::<ClusterScene>()
        .inventory = Some(Arc::new(
        Inventory::build(package.clone(), layout, &[], 0).unwrap(),
    ));
    render.run_system_once(publish).unwrap();
    assert!(
        !render
            .resource::<MainWorld>()
            .resource::<ClusterLoadingGate>()
            .world_ready(42)
    );
    assert!(render.resource::<Producer>().revision.is_none());
    render
        .resource_mut::<MainWorld>()
        .resource_mut::<ClusterScene>()
        .inventory = Some(inventory.clone());
    let restored = render
        .resource_mut::<Producer>()
        .observe(
            catalog_key(),
            [0; 9],
            consumers,
            || panic!("publication rejection can reuse contributions"),
            |_| (true, true, false, true),
        )
        .expect("restoring the exact same inventory Arc must recompute aggregate proof");
    render.resource_mut::<Producer>().pending = Some(Snapshot {
        package,
        inventory,
        layout,
        generation,
        frame: 2,
        worlds: restored,
    });
    render.run_system_once(publish).unwrap();
    assert!(
        render
            .resource::<MainWorld>()
            .resource::<ClusterLoadingGate>()
            .world_ready(42)
    );
}
