// Included inside readiness_bridge.rs's test module.

use std::collections::BTreeSet;

struct InventoryFixture {
    directory: tempfile::TempDir,
    package: Arc<Certificate>,
    inventory: Arc<Inventory>,
    instances: Vec<ClusterInstance>,
    first: RecordKey,
    second: RecordKey,
    producer: Producer,
    key: ReadinessKey,
    consumers: Vec<Consumer>,
    runtime_snapshots: std::cell::Cell<usize>,
}

impl InventoryFixture {
    fn new() -> Self {
        Self::from_directory(crate::cluster::reader::producer_fixture_tests::published_for_loader())
    }

    fn from_directory(directory: tempfile::TempDir) -> Self {
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let first = *package
            .records()
            .min_by_key(|(_, record)| (record.worldspace, record.reference))
            .unwrap()
            .0;
        let first_world = package.record(&first).unwrap().worldspace;
        let second = *package
            .records()
            .find(|(_, record)| record.worldspace != first_world)
            .unwrap()
            .0;
        let second_world = package.record(&second).unwrap().worldspace;
        let object = |record, batch| ClusterInstance {
            identity: ClusterInstanceIdentity::Object(record),
            mesh: 0,
            batch,
            world_from_local: Mat4::IDENTITY,
            source_worldspace: u32::MAX,
        };
        // No prepared meshes or resident blocks: all pending instances are dependencies.
        let instances = vec![
            object(first, 0),
            object(first, 0),
            object(second, 0),
            ClusterInstance {
                identity: ClusterInstanceIdentity::Terrain {
                    block: 0,
                    worldspace: first_world,
                    tier: 0,
                    anchor: [0, 0],
                },
                ..object(first, 0)
            },
            object(first, 1),
            object(second, 2),
        ];
        let key = catalog_key();
        let inventory = Arc::new(
            Inventory::build(package.clone(), key.catalog.layout_scope, &instances, 3).unwrap(),
        );
        let mut object_view = consumer(1, false, Some(Arc::new(vec![true; 3])));
        object_view.world = first_world;
        object_view.layers = RenderLayers::layer(crate::render::WORLD_LAYER);
        let mut second_view = consumer(2, false, Some(Arc::new(vec![true; 3])));
        second_view.world = second_world;
        Self {
            directory,
            package,
            inventory,
            instances,
            first,
            second,
            producer: Producer::default(),
            key,
            consumers: vec![object_view, second_view],
            runtime_snapshots: std::cell::Cell::new(0),
        }
    }

    fn step(
        &mut self,
        refused: &BTreeSet<RecordKey>,
        runtime: &BTreeSet<u32>,
    ) -> Option<HashMap<u32, ClusterWorldReadiness>> {
        if !self.producer.validate_inventory(
            &self.inventory,
            &self.package,
            self.key.catalog.layout_scope,
            self.key.catalog.scene_changed,
            &self.instances,
        ) {
            self.producer.withdraw();
            return None;
        }
        let inventory = &self.inventory;
        let package = &self.package;
        let key = self.key;
        let runtime_snapshots = &self.runtime_snapshots;
        self.producer.observe_with(
            key,
            [0; 9],
            self.consumers.clone(),
            |producer| {
                producer.update_inventory(
                    inventory,
                    package,
                    key.catalog.refusal_revision,
                    refused,
                    key.catalog.runtime_state_revision,
                    || {
                        runtime_snapshots.set(runtime_snapshots.get() + 1);
                        runtime.clone()
                    },
                )
            },
            |_| (true, true, false, true),
        )
    }

    fn worlds(&self) -> (u32, u32) {
        (
            self.package.record(&self.first).unwrap().worldspace,
            self.package.record(&self.second).unwrap().worldspace,
        )
    }
}

#[test]
fn one_refusal_rebuilds_only_inverse_batches_including_pending_occurrences() {
    let mut fixture = InventoryFixture::new();
    let (first_world, second_world) = fixture.worlds();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    let initial = fixture.step(&no_refusals, &no_runtime).unwrap();
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 3,
            visited_instances: 6
        }
    );
    assert_eq!(initial[&first_world].needed_batches, 3); // two object batches plus terrain.
    assert_eq!(initial[&second_world].needed_batches, 2);
    fixture.key.catalog.refusal_revision = Some(24);
    let changed = fixture
        .step(&BTreeSet::from([fixture.first]), &no_runtime)
        .unwrap();
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 5,
            visited_instances: 11
        }
    );
    assert_eq!(changed[&first_world].needed_batches, 1); // terrain survives.
    assert_eq!(changed[&second_world].needed_batches, 2);
    assert!(!fixture.producer.needed_catalog[&first_world].contains_key(&(1, false)));
}

#[test]
fn runtime_delta_rebuilds_union_once_and_unknown_references_visit_nothing() {
    let mut fixture = InventoryFixture::new();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    fixture.step(&no_refusals, &no_runtime).unwrap();
    let a = fixture.package.record(&fixture.first).unwrap().reference;
    let b = fixture.package.record(&fixture.second).unwrap().reference;
    fixture.key.catalog.runtime_state_revision = Some(32);
    let changed = fixture.step(&no_refusals, &BTreeSet::from([a, b])).unwrap();
    // Shared batch zero is rebuilt once, not once per changed reference.
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 6,
            visited_instances: 12
        }
    );
    let (wa, wb) = fixture.worlds();
    assert_eq!(changed[&wa].failed_batches, 2);
    assert_eq!(changed[&wb].failed_batches, 2);
    assert!(!fixture.producer.needed_catalog[&wa][&(0, true)].invalid_owned_state);

    let work = fixture.producer.inventory_work;
    fixture.key.catalog.runtime_state_revision = Some(33);
    fixture
        .step(&no_refusals, &BTreeSet::from([a, b, u32::MAX]))
        .unwrap();
    assert_eq!(fixture.producer.inventory_work, work);
    fixture.key.catalog.runtime_state_revision = Some(34);
    fixture.step(&no_refusals, &BTreeSet::from([a, b])).unwrap();
    assert_eq!(fixture.producer.inventory_work, work);
    fixture.key.catalog.runtime_state_revision = Some(35);
    let restored_b = fixture.step(&no_refusals, &BTreeSet::from([a])).unwrap();
    assert_eq!(restored_b[&wa].failed_batches, 2);
    assert_eq!(restored_b[&wb].failed_batches, 0);
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 8,
            visited_instances: 17
        }
    );
    fixture.key.catalog.runtime_state_revision = Some(36);
    let restored_a = fixture.step(&no_refusals, &no_runtime).unwrap();
    assert_eq!(restored_a[&wa].failed_batches, 0);
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 10,
            visited_instances: 22
        }
    );
}

#[test]
fn removing_last_object_contributors_clears_invalid_groups_and_recovery_is_symmetric() {
    let mut fixture = InventoryFixture::new();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    let a = fixture.package.record(&fixture.first).unwrap().reference;
    let b = fixture.package.record(&fixture.second).unwrap().reference;
    let runtime = BTreeSet::from([a, b]);
    fixture.step(&no_refusals, &runtime).unwrap();
    let (wa, wb) = fixture.worlds();
    fixture.key.catalog.refusal_revision = Some(24);
    let one = fixture
        .step(&BTreeSet::from([fixture.first]), &runtime)
        .unwrap();
    assert_eq!(one[&wa].failed_batches, 0);
    assert_eq!(one[&wa].needed_batches, 1);
    assert_eq!(one[&wb].failed_batches, 2);
    fixture.key.catalog.refusal_revision = Some(25);
    let both = fixture
        .step(&BTreeSet::from([fixture.first, fixture.second]), &runtime)
        .unwrap();
    assert_eq!(both[&wa].needed_batches, 1);
    assert_eq!(both[&wb].needed_batches, 0);
    assert!(!fixture.producer.needed_catalog.contains_key(&wb));
    fixture.key.catalog.refusal_revision = Some(26);
    fixture.key.catalog.runtime_state_revision = Some(32);
    let recovered = fixture.step(&no_refusals, &no_runtime).unwrap();
    assert_eq!(recovered[&wa].needed_batches, 3);
    assert_eq!(recovered[&wb].needed_batches, 2);
    assert_eq!(recovered[&wa].failed_batches, 0);
    assert_eq!(recovered[&wb].failed_batches, 0);
}

#[test]
fn settled_producer_skips_semantic_sets_index_validation_and_batch_visits() {
    let mut fixture = InventoryFixture::new();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    fixture.step(&no_refusals, &no_runtime).unwrap();
    let work = fixture.producer.inventory_work;
    let validation_walks = fixture.producer.validation_walks;
    assert_eq!(fixture.runtime_snapshots.get(), 1);
    assert!(fixture.step(&no_refusals, &no_runtime).is_none());
    assert_eq!(fixture.producer.inventory_work, work);
    assert_eq!(fixture.producer.validation_walks, validation_walks);
    // A material-only aggregate refresh still performs no membership work.
    fixture.key.materials_revision += 1;
    fixture.step(&no_refusals, &no_runtime).unwrap();
    assert_eq!(fixture.producer.inventory_work, work);
    assert_eq!(fixture.producer.validation_walks, validation_walks);
    // A scene tick may reflect transforms/origin; validate exact identities once,
    // then reuse contributions when the immutable inventory allocation survived.
    fixture.instances[0].world_from_local = Mat4::from_translation(Vec3::X);
    fixture.instances[0].mesh = 17;
    fixture.instances[0].source_worldspace = 42;
    fixture.key.catalog.scene_changed = Tick::new(6);
    fixture.step(&no_refusals, &no_runtime).unwrap();
    assert_eq!(fixture.producer.inventory_work, work);
    assert_eq!(fixture.producer.validation_walks, validation_walks + 1);
    assert_eq!(fixture.runtime_snapshots.get(), 1);
}

#[test]
fn optional_revisions_and_effective_runtime_removal_update_only_affected_batches() {
    let mut fixture = InventoryFixture::new();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    fixture.key.catalog.refusal_revision = None;
    fixture.key.catalog.runtime_state_revision = None;
    fixture.step(&no_refusals, &no_runtime).unwrap();
    let reference = fixture.package.record(&fixture.first).unwrap().reference;
    fixture.key.catalog.refusal_revision = Some(0);
    fixture.key.catalog.runtime_state_revision = Some(0);
    let changed = fixture
        .step(
            &BTreeSet::from([fixture.first]),
            &BTreeSet::from([reference]),
        )
        .unwrap();
    let (world, _) = fixture.worlds();
    assert_eq!(changed[&world].needed_batches, 1);
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 5,
            visited_instances: 11
        }
    );
    fixture.key.catalog.refusal_revision = None;
    fixture.key.catalog.runtime_state_revision = None;
    let restored = fixture.step(&no_refusals, &no_runtime).unwrap();
    assert_eq!(restored[&world].needed_batches, 3);
    assert_eq!(restored[&world].failed_batches, 0);
}

#[test]
fn ordered_validation_precedes_replay_persists_rejection_and_recovers_same_arc() {
    let mut fixture = InventoryFixture::new();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    fixture.step(&no_refusals, &no_runtime).unwrap();
    let inventory = fixture.inventory.clone();
    let original = fixture.instances.clone();
    // Neither the aggregate key nor semantic revision changes beyond the scene tick.
    fixture.key.catalog.scene_changed = Tick::new(6);
    fixture.instances[0].batch = 2;
    assert!(fixture.step(&no_refusals, &no_runtime).is_none());
    assert!(fixture.producer.revision.is_none());
    let validations = fixture.producer.validation_walks;
    assert!(fixture.step(&no_refusals, &no_runtime).is_none());
    assert_eq!(fixture.producer.validation_walks, validations);
    fixture.instances = original;
    fixture.key.catalog.scene_changed = Tick::new(7);
    assert!(fixture.step(&no_refusals, &no_runtime).is_some());
    assert!(Arc::ptr_eq(&inventory, &fixture.inventory));
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 3,
            visited_instances: 6
        }
    );
    for mutation in 0..3 {
        let original = fixture.instances.clone();
        match mutation {
            0 => fixture.instances[0].identity = ClusterInstanceIdentity::Object(fixture.second),
            1 => fixture.instances.swap(0, 2),
            _ => {
                fixture.instances.pop();
            }
        }
        fixture.key.catalog.scene_changed = Tick::new(8 + mutation * 2);
        assert!(
            fixture.step(&no_refusals, &no_runtime).is_none(),
            "mutation={mutation}"
        );
        assert!(
            fixture.step(&no_refusals, &no_runtime).is_none(),
            "repeated mutation={mutation}"
        );
        fixture.instances = original;
        fixture.key.catalog.scene_changed = Tick::new(9 + mutation * 2);
        assert!(
            fixture.step(&no_refusals, &no_runtime).is_some(),
            "restore mutation={mutation}"
        );
        assert_eq!(
            fixture.producer.inventory_work,
            InventoryWork {
                rebuilt_batches: 3,
                visited_instances: 6
            }
        );
    }
}

#[test]
fn inventory_replacement_rebuilds_even_when_numeric_keys_ticks_and_revisions_match() {
    let mut fixture = InventoryFixture::new();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    fixture.step(&no_refusals, &no_runtime).unwrap();
    fixture.instances[0].batch = 2;
    fixture.inventory = Arc::new(
        Inventory::build(
            fixture.package.clone(),
            fixture.key.catalog.layout_scope,
            &fixture.instances,
            3,
        )
        .unwrap(),
    );
    assert!(fixture.step(&no_refusals, &no_runtime).is_some());
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 6,
            visited_instances: 12
        }
    );
}

#[test]
fn inventory_rejects_equal_bytes_in_another_certificate_and_wrong_layout() {
    let mut fixture = InventoryFixture::new();
    let no_refusals = BTreeSet::new();
    let no_runtime = BTreeSet::new();
    fixture.step(&no_refusals, &no_runtime).unwrap();
    let package = fixture.package.clone();
    fixture.package = Arc::new(
        crate::cluster::overlay_fixture::open_fixture(fixture.directory.path(), false).unwrap(),
    );
    assert!(fixture.step(&no_refusals, &no_runtime).is_none());
    fixture.package = package;
    assert!(fixture.step(&no_refusals, &no_runtime).is_some());
    fixture.key.catalog.layout_scope += 1;
    assert!(fixture.step(&no_refusals, &no_runtime).is_none());
    fixture.key.catalog.layout_scope -= 1;
    assert!(fixture.step(&no_refusals, &no_runtime).is_some());
}

#[test]
fn runtime_invalidity_is_or_of_surviving_objects_within_each_world_and_domain() {
    let mut fixture = InventoryFixture::from_directory(
        crate::cluster::reader::producer_fixture_tests::published_for_readiness(),
    );
    let (world, other_world) = fixture.worlds();
    let first_reference = fixture.package.record(&fixture.first).unwrap().reference;
    let (third, third_reference) = fixture
        .package
        .records()
        .find(|(_, record)| record.worldspace == world && record.reference != first_reference)
        .map(|(key, record)| (*key, record.reference))
        .unwrap();
    // Put the initially valid contributor last: assignment instead of OR would
    // erase the earlier invalid contribution in this same (world, batch, domain).
    fixture.instances.push(ClusterInstance {
        identity: ClusterInstanceIdentity::Object(third),
        batch: 0,
        ..fixture.instances[0]
    });
    fixture.inventory = Arc::new(
        Inventory::build(
            fixture.package.clone(),
            fixture.key.catalog.layout_scope,
            &fixture.instances,
            3,
        )
        .unwrap(),
    );
    let no_refusals = BTreeSet::new();
    let runtime = BTreeSet::from([first_reference]);
    let initial = fixture.step(&no_refusals, &runtime).unwrap();
    assert_eq!(initial[&world].failed_batches, 2);
    assert_eq!(initial[&other_world].failed_batches, 0);
    assert!(!fixture.producer.needed_catalog[&world][&(0, true)].invalid_owned_state);
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 3,
            visited_instances: 7
        }
    );

    fixture.key.catalog.refusal_revision = Some(24);
    let removed_invalid = fixture
        .step(&BTreeSet::from([fixture.first]), &runtime)
        .unwrap();
    assert_eq!(removed_invalid[&world].needed_batches, 2);
    assert_eq!(removed_invalid[&world].failed_batches, 0);
    assert_eq!(
        fixture.producer.inventory_work,
        InventoryWork {
            rebuilt_batches: 5,
            visited_instances: 13
        }
    );

    fixture.key.catalog.runtime_state_revision = Some(32);
    let remaining_invalid = fixture
        .step(
            &BTreeSet::from([fixture.first]),
            &BTreeSet::from([third_reference]),
        )
        .unwrap();
    assert_eq!(remaining_invalid[&world].failed_batches, 1);
    fixture.key.catalog.refusal_revision = Some(25);
    let restored = fixture
        .step(&no_refusals, &BTreeSet::from([third_reference]))
        .unwrap();
    assert_eq!(restored[&world].needed_batches, 3);
    assert_eq!(restored[&world].failed_batches, 1);
    fixture.key.catalog.refusal_revision = Some(26);
    let removed_last_invalid = fixture
        .step(&BTreeSet::from([third]), &BTreeSet::from([third_reference]))
        .unwrap();
    assert_eq!(removed_last_invalid[&world].needed_batches, 3);
    assert_eq!(removed_last_invalid[&world].failed_batches, 0);
}
