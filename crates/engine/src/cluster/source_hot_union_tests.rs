type HotUnionReference<'w, 's> = Query<'w, 's, Entity, (SourceObservationDomain, HotFactChanges)>;

#[derive(SystemParam)]
struct HotUnionQueries<'w, 's> {
    hot: HotFactQueries<'w, 's>,
    flat: ChangedFlatHotFacts<'w, 's>,
    native: HotUnionReference<'w, 's>,
}

type HotUnionConsumer = bevy::ecs::system::SystemState<HotUnionQueries<'static, 'static>>;

#[derive(Component)]
#[component(storage = "SparseSet")]
struct HotUnionHidden;
#[derive(Component)]
#[component(storage = "SparseSet")]
struct HotUnionLateHidden;

fn assert_hot_union(consumer: &mut HotUnionConsumer, world: &World, expected: &[Entity]) -> u64 {
    let query = consumer.get(world).unwrap();
    let (original, probes) = query.hot.iter(None);
    let (_, admitted) = probes.work();
    let original: EntityHashSet = original.collect();
    // Deliberately exhaustive: a new macro field must force an explicit audit
    // of iter_other, the independent native filter and this mutation matrix.
    let HotFactProbes {
        pose,
        flat_primitive,
        flat_pose,
        visibility,
        inherited,
        layers,
        standard,
    } = probes;
    assert_eq!(
        admitted,
        [
            pose,
            flat_primitive,
            flat_pose,
            visibility,
            inherited,
            layers,
            standard
        ]
        .into_iter()
        .filter(|probe| probe.changed)
        .count() as u64
    );
    let mut fused: EntityHashSet = query.hot.iter_other(probes).collect();
    if flat_primitive.changed || flat_pose.changed {
        fused.extend(query.flat.iter().map(|(entity, _)| entity));
    }
    let native: EntityHashSet = query.native.iter().collect();
    assert_eq!(
        original, native,
        "original component admissions must match native authority"
    );
    assert_eq!(
        fused, native,
        "fused lane union iter_other must cover every native hot change"
    );
    assert_eq!(native, expected.iter().copied().collect());
    admitted
}

fn mutate_hot_union_lane(world: &mut World, entity: Entity, lane: usize) {
    match lane {
        0 => world.get_mut::<Transform>(entity).unwrap().translation += Vec3::X,
        1 => {
            world
                .get_mut::<FlatSourcePrimitive>(entity)
                .unwrap()
                .model_matrix = Mat4::ZERO
        }
        2 => {
            *world.get_mut::<GlobalTransform>(entity).unwrap() =
                GlobalTransform::from_translation(Vec3::X)
        }
        3 => *world.get_mut::<Visibility>(entity).unwrap() = Visibility::Hidden,
        4 => *world.get_mut::<InheritedVisibility>(entity).unwrap() = InheritedVisibility::HIDDEN,
        5 => *world.get_mut::<RenderLayers>(entity).unwrap() = RenderLayers::from_layers(&[70]),
        // Equal raw replacements still mark the component changed. Asset loading
        // and material payloads are outside this native admission contract.
        6 => {
            *world
                .get_mut::<MeshMaterial3d<StandardMaterial>>(entity)
                .unwrap() = MeshMaterial3d::default()
        }
        _ => unreachable!(),
    }
}

#[test]
fn semantic_hot_fused_and_other_union_matches_native_over_every_raw_lane() {
    let mut world = World::new();
    world.register_disabling_component::<HotUnionHidden>();
    world.register_disabling_component::<FormId>();
    world.register_disabling_component::<RenderLayers>();
    let fields = || {
        (
            Transform::IDENTITY,
            GlobalTransform::IDENTITY,
            Visibility::Inherited,
            InheritedVisibility::VISIBLE,
            RenderLayers::layer(3),
            MeshMaterial3d::<StandardMaterial>::default(),
            FormId(7),
        )
    };
    let metadata = FlatSourcePrimitive {
        model_matrix: Mat4::IDENTITY,
        expected_global: GlobalTransform::IDENTITY,
    };
    let flat = world.spawn((fields(), metadata.clone())).id();
    let plain = world.spawn(fields()).id();
    let hidden = world
        .spawn((fields(), metadata.clone(), HotUnionHidden))
        .id();
    let missing_global = world.spawn(metadata.clone()).id();
    let mut consumer = HotUnionConsumer::new(&mut world);
    assert_hot_union(&mut consumer, &world, &[flat, plain, missing_global]);
    assert_hot_union(&mut consumer, &world, &[]);
    for lane in 0..7 {
        mutate_hot_union_lane(&mut world, flat, lane);
        assert_hot_union(&mut consumer, &world, &[flat]);
        assert_hot_union(&mut consumer, &world, &[]);
        if lane != 1 {
            mutate_hot_union_lane(&mut world, plain, lane);
            let plain_expected = [plain];
            assert_hot_union(
                &mut consumer,
                &world,
                if lane == 2 { &[] } else { &plain_expected },
            );
        }
        mutate_hot_union_lane(&mut world, hidden, lane);
        assert_hot_union(&mut consumer, &world, &[]);
    }
    // Overlapping branches share one native observation window.
    for lane in 0..7 {
        mutate_hot_union_lane(&mut world, flat, lane);
    }
    assert_hot_union(&mut consumer, &world, &[flat]);
    world
        .get_mut::<FlatSourcePrimitive>(missing_global)
        .unwrap()
        .model_matrix = Mat4::ZERO;
    assert_hot_union(&mut consumer, &world, &[missing_global]);
    world.entity_mut(flat).remove::<GlobalTransform>();
    assert_hot_union(&mut consumer, &world, &[]);
    mutate_hot_union_lane(&mut world, flat, 1);
    assert_hot_union(&mut consumer, &world, &[flat]);
    world.entity_mut(flat).insert(GlobalTransform::IDENTITY);
    assert_hot_union(&mut consumer, &world, &[flat]);
    world.entity_mut(flat).remove::<FlatSourcePrimitive>();
    mutate_hot_union_lane(&mut world, flat, 2);
    assert_hot_union(&mut consumer, &world, &[]);
    world.entity_mut(flat).insert(metadata.clone());
    assert_hot_union(&mut consumer, &world, &[flat]);
    world.entity_mut(hidden).remove::<HotUnionHidden>();
    assert_hot_union(&mut consumer, &world, &[]);
    mutate_hot_union_lane(&mut world, hidden, 6);
    assert_hot_union(&mut consumer, &world, &[hidden]);
    world.register_disabling_component::<HotUnionLateHidden>();
    let late = world.spawn((fields(), metadata, HotUnionLateHidden)).id();
    assert_hot_union(&mut consumer, &world, &[late]);
    let mut fresh = HotUnionConsumer::new(&mut world);
    assert_hot_union(&mut fresh, &world, &[flat, plain, hidden, missing_global]);
    mutate_hot_union_lane(&mut world, late, 6);
    assert_hot_union(&mut consumer, &world, &[late]);
    assert_hot_union(&mut fresh, &world, &[]);
    world.despawn(flat);
    assert_hot_union(&mut consumer, &world, &[]);
}

#[test]
fn semantic_hot_union_keeps_selective_dense_probes_and_quiet_windows() {
    let mut world = World::new();
    let fields = || {
        (
            Transform::IDENTITY,
            GlobalTransform::IDENTITY,
            Visibility::Inherited,
            InheritedVisibility::VISIBLE,
            RenderLayers::layer(3),
            MeshMaterial3d::<StandardMaterial>::default(),
        )
    };
    let flat = world
        .spawn((
            fields(),
            FlatSourcePrimitive {
                model_matrix: Mat4::IDENTITY,
                expected_global: GlobalTransform::IDENTITY,
            },
        ))
        .id();
    let plain = world.spawn(fields()).id();
    let mut consumer = HotUnionConsumer::new(&mut world);
    assert_eq!(assert_hot_union(&mut consumer, &world, &[flat, plain]), 7);
    assert_eq!(assert_hot_union(&mut consumer, &world, &[]), 0);
    for lane in 0..7 {
        mutate_hot_union_lane(&mut world, flat, lane);
        assert_eq!(assert_hot_union(&mut consumer, &world, &[flat]), 1);
        assert_eq!(assert_hot_union(&mut consumer, &world, &[]), 0);
        if lane != 1 {
            mutate_hot_union_lane(&mut world, plain, lane);
            if lane == 2 {
                assert_eq!(assert_hot_union(&mut consumer, &world, &[]), 0);
            } else {
                assert_eq!(assert_hot_union(&mut consumer, &world, &[plain]), 1);
            }
            assert_eq!(assert_hot_union(&mut consumer, &world, &[]), 0);
        }
    }
}
