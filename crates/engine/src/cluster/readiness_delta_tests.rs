// Production producer path, with immutable fixture inventory and no GPU device.
impl InventoryFixture {
    fn delta_step(
        &mut self,
        refused: &BTreeSet<RecordKey>,
        runtime: &BTreeSet<u32>,
        changed: BTreeSet<usize>,
        facts: impl Fn(usize) -> (bool, bool, bool, bool),
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
        let key = self.key;
        let inventory = &self.inventory;
        let package = &self.package;
        self.producer.observe_incremental(
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
                    || runtime.clone(),
                )
            },
            changed,
            false,
            |_, batch| facts(batch),
        )
    }
}

#[test]
fn production_delta_path_ignores_broad_epochs_and_validated_metadata_ticks() {
    let mut fixture = InventoryFixture::new();
    let empty = BTreeSet::new();
    let no_runtime = BTreeSet::<u32>::new();
    fixture
        .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| {
            (true, true, false, true)
        })
        .unwrap();
    fixture.producer.aggregate.visits = 0;
    fixture.key.materials_revision += 1;
    fixture.key.catalog.scene_changed = Tick::new(44);
    assert!(
        fixture
            .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| panic!(
                "metadata/global material epoch is not a dependency"
            ))
            .is_none()
    );
    fixture.key.catalog.runtime_state_revision = Some(99);
    assert!(
        fixture
            .delta_step(
                &empty,
                &BTreeSet::from([u32::MAX]),
                BTreeSet::new(),
                |_| panic!("unknown runtime reference")
            )
            .is_none()
    );
    assert_eq!(fixture.producer.aggregate.visits, 0);
}

#[test]
fn production_fallback_only_transition_and_recovery_touch_one_needed_batch() {
    let mut fixture = InventoryFixture::new();
    let empty = BTreeSet::new();
    let no_runtime = BTreeSet::<u32>::new();
    let (world, other) = fixture.worlds();
    fixture
        .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| {
            (true, false, false, true)
        })
        .unwrap();
    fixture.producer.aggregate.visits = 0;
    let failed = fixture
        .delta_step(&empty, &no_runtime, BTreeSet::from([1]), |batch| {
            assert_eq!(batch, 1);
            (true, false, true, true)
        })
        .unwrap();
    assert_eq!(failed[&world].failed_batches, 1);
    assert_eq!(failed[&other].failed_batches, 0);
    assert_eq!(fixture.producer.aggregate.visits, 1);
    let recovered = fixture
        .delta_step(&empty, &no_runtime, BTreeSet::from([1]), |_| {
            (true, false, false, true)
        })
        .unwrap();
    assert_eq!(recovered[&world].failed_batches, 0);
    assert_eq!(fixture.producer.aggregate.visits, 2);
}

#[test]
fn identical_inventory_replacement_republishes_current_provenance() {
    let mut fixture = InventoryFixture::new();
    let empty = BTreeSet::new();
    let no_runtime = BTreeSet::<u32>::new();
    let initial = fixture
        .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| {
            (true, true, false, true)
        })
        .unwrap();
    fixture.inventory = Arc::new(
        Inventory::build(
            fixture.package.clone(),
            fixture.key.catalog.layout_scope,
            &fixture.instances,
            3,
        )
        .unwrap(),
    );
    let current = fixture
        .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| {
            (true, true, false, true)
        })
        .expect("new inventory allocation requires current publication");
    assert_eq!(initial, current);
    assert!(
        fixture
            .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| panic!(
                "settled replacement"
            ))
            .is_none()
    );
}

#[test]
fn production_survivor_or_does_not_dirty_an_unchanged_world_domain_group() {
    let mut fixture = InventoryFixture::from_directory(
        crate::cluster::reader::producer_fixture_tests::published_for_readiness(),
    );
    let (world, other) = fixture.worlds();
    let first_ref = fixture.package.record(&fixture.first).unwrap().reference;
    let (third, third_ref) = fixture
        .package
        .records()
        .find(|(_, record)| record.worldspace == world && record.reference != first_ref)
        .map(|(key, record)| (*key, record.reference))
        .unwrap();
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
    let empty = BTreeSet::new();
    let no_runtime = BTreeSet::<u32>::new();
    let first = fixture
        .delta_step(
            &empty,
            &BTreeSet::from([first_ref, third_ref]),
            BTreeSet::new(),
            |_| (true, true, false, true),
        )
        .unwrap();
    assert_eq!(first[&world].failed_batches, 2);
    fixture.producer.aggregate.visits = 0;
    fixture.key.catalog.runtime_state_revision = Some(32);
    let one_survivor = fixture
        .delta_step(
            &empty,
            &BTreeSet::from([third_ref]),
            BTreeSet::new(),
            |batch| {
                assert_eq!(batch, 1, "batch zero still has an invalid survivor");
                (true, true, false, true)
            },
        )
        .unwrap();
    assert_eq!(one_survivor[&world].failed_batches, 1);
    assert_eq!(one_survivor[&other].failed_batches, 0);
    assert_eq!(fixture.producer.aggregate.visits, 1);
    fixture.key.catalog.runtime_state_revision = Some(33);
    let none = fixture
        .delta_step(&empty, &no_runtime, BTreeSet::new(), |batch| {
            assert_eq!(batch, 0);
            (true, true, false, true)
        })
        .unwrap();
    assert_eq!(none[&world].failed_batches, 0);
    assert_eq!(none[&other].failed_batches, 0);
}

#[test]
fn absent_required_resources_withdraw_instead_of_skipping_the_observer() {
    use bevy::ecs::system::RunSystemOnce;
    let mut world = World::new();
    let producer = Producer {
        revision: Some([0; 9]),
        ..default()
    };
    world.insert_resource(producer);
    world.run_system_once(observe).unwrap();
    let producer = world.resource::<Producer>();
    assert!(producer.revision.is_none());
    assert!(producer.withdrawal_pending);
    assert!(producer.pending.is_none());
}

#[test]
fn withdrawal_and_same_input_recovery_require_current_publication() {
    let mut fixture = InventoryFixture::new();
    let empty = BTreeSet::new();
    let no_runtime = BTreeSet::<u32>::new();
    let before = fixture
        .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| {
            (true, true, false, true)
        })
        .unwrap();
    fixture.producer.withdraw();
    let after = fixture
        .delta_step(&empty, &no_runtime, BTreeSet::new(), |_| {
            (true, true, false, true)
        })
        .expect("same scope recovery cannot replay withdrawn proof");
    assert_eq!(before, after);
    assert!(fixture.producer.revision.is_some());
}
