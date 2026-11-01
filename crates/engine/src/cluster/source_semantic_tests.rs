// Operation counts describe actual tuple access and normalization, not elapsed
// microseconds. The synthetic draw population is not the native route population.

#[test]
fn semantic_flat_crossing_fetches_tuples_once_without_random_gets() {
    let count = 1024;
    let (mut input, primitive, mut draws) = observation_population(count);
    observation_rebase(&mut input, &mut draws, IVec2::new(3, -5));
    input.assert_bound(primitive);
    let work = input.sources().last_work;
    assert_eq!(work.hot_fact_reads, count as u64 + 1);
    assert_eq!(work.hot_flat_model_checks, count as u64);
    assert_eq!(work.hot_flat_affine_checks, count as u64);
    assert_eq!(work.hot_flat_affine_compares, count as u64);
    assert_eq!(work.hot_cell_checks, 1);
    assert_eq!(
        work.hot_tuple_gets, 1,
        "only the non-flat cell needs a random tuple lookup"
    );
    assert_eq!(work.hot_tuple_rows, count as u64);
    assert_eq!(work.hot_entities_yielded, count as u64 + 1);
    assert_full_oracle(&mut input);
}

// Include inside handoff::extract_source_tests with the existing fixture/helpers.
// Nonfinite canonicalization tests remain authoritative; do not duplicate them.
// Observe counters and run the full oracle BEFORE repairing an authored edit.

#[derive(Component)]
struct SemanticR7Hidden;
#[derive(Component)]
#[component(storage = "SparseSet")]
struct SemanticR7LateHidden;

type SemanticR7NativeGlobal<'w, 's> = Query<
    'w,
    's,
    (Entity, HotFactState<'static>),
    (
        SourceObservationDomain,
        With<FlatSourcePrimitive>,
        Changed<GlobalTransform>,
    ),
>;

#[derive(SystemParam)]
struct SemanticR7FlatQueries<'w, 's> {
    fused: ChangedFlatHotFacts<'w, 's>,
    metadata: Query<
        'w,
        's,
        (Entity, HotFactState<'static>),
        (SourceObservationDomain, Changed<FlatSourcePrimitive>),
    >,
    global: SemanticR7NativeGlobal<'w, 's>,
}

type SemanticR7Consumer = bevy::ecs::system::SystemState<SemanticR7FlatQueries<'static, 'static>>;

fn semantic_r7_compare(consumer: &mut SemanticR7Consumer, world: &World, expected: &[Entity]) {
    // Compare every optional value, including exact flat metadata and affine,
    // rather than normalized HotFacts (which intentionally collapse bad proofs).
    let snapshot = |s: HotFactState<'_>| {
        (
            (
                s.0.copied(),
                s.1.copied(),
                s.2.copied(),
                s.3.map(|v| v.get()),
                s.4.cloned(),
            ),
            (
                s.5.map(|m| m.id()),
                s.6.copied(),
                s.7.map(|f| (f.model_matrix, f.expected_global)),
                s.8.copied(),
            ),
        )
    };
    let queries = consumer.get(world).unwrap();
    let fused: EntityHashMap<_> = queries
        .fused
        .iter()
        .map(|(e, s)| (e, snapshot(s)))
        .collect();
    let original: EntityHashMap<_> = queries
        .metadata
        .iter()
        .chain(queries.global.iter())
        .map(|(e, s)| (e, snapshot(s)))
        .collect();
    assert_eq!(
        fused, original,
        "fused rows must equal the native two-branch union"
    );
    assert_eq!(
        queries.fused.iter().count(),
        fused.len(),
        "overlap yields one row"
    );
    assert_eq!(
        fused.keys().copied().collect::<EntityHashSet>(),
        expected.iter().copied().collect::<EntityHashSet>()
    );
}

#[test]
fn semantic_r7_fused_rows_match_native_union_with_persistent_captured_policy() {
    let mut world = World::new();
    world.register_disabling_component::<SemanticR7Hidden>();
    world.register_disabling_component::<FormId>();
    world.register_disabling_component::<RenderLayers>();
    world.register_disabling_component::<FlatSourcePrimitive>();
    world.register_disabling_component::<GlobalTransform>();
    let metadata = FlatSourcePrimitive {
        model_matrix: Mat4::IDENTITY,
        expected_global: GlobalTransform::IDENTITY,
    };
    // No Mesh3d, owner, or index: the native iterator must still yield these rows.
    let flat = world
        .spawn((
            metadata.clone(),
            GlobalTransform::IDENTITY,
            Transform::IDENTITY,
            SourceNodePose(Transform::IDENTITY),
            Visibility::Inherited,
            InheritedVisibility::VISIBLE,
            RenderLayers::layer(3),
            MeshMaterial3d::<StandardMaterial>(Handle::default()),
            StreamedCellKey(CellKey::Interior(7)),
            FormId(7),
        ))
        .id();
    let missing_global = world.spawn(metadata.clone()).id();
    let hidden = world
        .spawn((
            metadata.clone(),
            GlobalTransform::IDENTITY,
            SemanticR7Hidden,
        ))
        .id();
    let unrelated = world.spawn(GlobalTransform::IDENTITY).id();
    let mut consumer = SemanticR7Consumer::new(&mut world);
    semantic_r7_compare(&mut consumer, &world, &[flat, missing_global]);
    semantic_r7_compare(&mut consumer, &world, &[]);
    *world.get_mut::<GlobalTransform>(flat).unwrap() = GlobalTransform::from_translation(Vec3::X);
    semantic_r7_compare(&mut consumer, &world, &[flat]);
    world
        .get_mut::<FlatSourcePrimitive>(missing_global)
        .unwrap()
        .model_matrix = Mat4::from_translation(Vec3::Y);
    semantic_r7_compare(&mut consumer, &world, &[missing_global]);
    world
        .get_mut::<FlatSourcePrimitive>(flat)
        .unwrap()
        .expected_global = GlobalTransform::from_translation(Vec3::Z);
    *world.get_mut::<GlobalTransform>(flat).unwrap() = GlobalTransform::from_translation(Vec3::Z);
    *world.get_mut::<GlobalTransform>(unrelated).unwrap() =
        GlobalTransform::from_translation(Vec3::Z);
    world
        .entity_mut(flat)
        .insert((Visibility::Hidden, RenderLayers::layer(5)));
    semantic_r7_compare(&mut consumer, &world, &[flat]);
    world.entity_mut(flat).remove::<GlobalTransform>();
    world
        .get_mut::<FlatSourcePrimitive>(flat)
        .unwrap()
        .model_matrix = Mat4::from_scale(Vec3::splat(2.0));
    semantic_r7_compare(&mut consumer, &world, &[flat]);
    world.entity_mut(flat).insert(GlobalTransform::IDENTITY);
    semantic_r7_compare(&mut consumer, &world, &[flat]);
    world.entity_mut(flat).remove::<FlatSourcePrimitive>();
    *world.get_mut::<GlobalTransform>(flat).unwrap() = GlobalTransform::from_translation(Vec3::X);
    semantic_r7_compare(&mut consumer, &world, &[]);
    world.entity_mut(flat).insert(metadata.clone());
    semantic_r7_compare(&mut consumer, &world, &[flat]);
    world
        .get_mut::<FlatSourcePrimitive>(hidden)
        .unwrap()
        .model_matrix = Mat4::ZERO;
    *world.get_mut::<GlobalTransform>(hidden).unwrap() = GlobalTransform::from_translation(Vec3::Y);
    semantic_r7_compare(&mut consumer, &world, &[]);

    // Registration cannot silently replace an existing query's captured policy.
    world.register_disabling_component::<SemanticR7LateHidden>();
    let late = world
        .spawn((metadata, GlobalTransform::IDENTITY, SemanticR7LateHidden))
        .id();
    semantic_r7_compare(&mut consumer, &world, &[late]);
    let mut fresh = SemanticR7Consumer::new(&mut world);
    semantic_r7_compare(&mut fresh, &world, &[flat, missing_global]);
    *world.get_mut::<GlobalTransform>(late).unwrap() = GlobalTransform::from_translation(Vec3::X);
    semantic_r7_compare(&mut consumer, &world, &[late]);
    semantic_r7_compare(&mut fresh, &world, &[]);
}

#[test]
fn semantic_r7_crossing_authored_edits_match_full_oracle_before_repair() {
    for variant in 0..4 {
        let (mut input, primitive, mut draws) = observation_population(8);
        let original_material = input
            .app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let replacement = input
            .app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        input.app.world_mut().entity_mut(primitive).insert((
            MeshMaterial3d(original_material.clone()),
            RenderLayers::layer(3),
        ));
        // Exercise native candidate accounting without pretending an unindexed
        // row costs no tuple fetch just because it needs no normalized snapshot.
        let loose = input
            .app
            .world_mut()
            .spawn((
                FlatSourcePrimitive {
                    model_matrix: Mat4::IDENTITY,
                    expected_global: GlobalTransform::IDENTITY,
                },
                GlobalTransform::IDENTITY,
            ))
            .id();
        draws.push((loose, GlobalTransform::IDENTITY));
        input.assert_bound(primitive);
        input.assert_bound(primitive);
        let model = input
            .app
            .world()
            .get::<FlatSourcePrimitive>(primitive)
            .unwrap()
            .model_matrix;
        observation_rebase(&mut input, &mut draws, IVec2::new(3, -5));
        let crossing_tick = input
            .app
            .world()
            .entity(primitive)
            .get_change_ticks::<GlobalTransform>()
            .unwrap()
            .changed;
        let world = input.app.world_mut();
        let authored_tick = match variant {
            0 => {
                world
                    .get_mut::<MeshMaterial3d<StandardMaterial>>(primitive)
                    .unwrap()
                    .0 = replacement;
                world
                    .entity(primitive)
                    .get_change_ticks::<MeshMaterial3d<StandardMaterial>>()
                    .unwrap()
                    .changed
            }
            1 => {
                *world.get_mut::<RenderLayers>(primitive).unwrap() = RenderLayers::layer(5);
                world
                    .entity(primitive)
                    .get_change_ticks::<RenderLayers>()
                    .unwrap()
                    .changed
            }
            2 => {
                world
                    .get_mut::<FlatSourcePrimitive>(primitive)
                    .unwrap()
                    .model_matrix = model * Mat4::from_translation(Vec3::X);
                world
                    .entity(primitive)
                    .get_change_ticks::<FlatSourcePrimitive>()
                    .unwrap()
                    .changed
            }
            3 => {
                *world.get_mut::<GlobalTransform>(primitive).unwrap() =
                    GlobalTransform::from_translation(Vec3::X) * draws[0].1;
                world
                    .entity(primitive)
                    .get_change_ticks::<GlobalTransform>()
                    .unwrap()
                    .changed
            }
            _ => unreachable!(),
        };
        assert_eq!(
            crossing_tick, authored_tick,
            "same native tick, variant {variant}"
        );
        input.extract();
        assert_delta_only(&input);
        let work = input.sources().last_work;
        assert!(
            work.changed_entities > 0,
            "authored variant {variant} must be observed"
        );
        assert!(
            work.hot_tuple_rows >= draws.len() as u64,
            "include unindexed native candidates"
        );
        assert!(
            work.hot_tuple_gets > 0,
            "the changed cell still needs a random tuple get"
        );
        assert!(work.hot_flat_model_checks <= work.hot_fact_reads);
        assert_eq!(work.hot_flat_model_checks, work.hot_flat_affine_checks);
        if variant >= 2 {
            assert!(!input.sources().bound.contains_key(&input.key));
        }
        assert_full_oracle(&mut input);

        // Repair only after comparing the first delta with the independent loops.
        let world = input.app.world_mut();
        match variant {
            0 => {
                world
                    .get_mut::<MeshMaterial3d<StandardMaterial>>(primitive)
                    .unwrap()
                    .0 = original_material
            }
            1 => *world.get_mut::<RenderLayers>(primitive).unwrap() = RenderLayers::layer(3),
            2 => {
                world
                    .get_mut::<FlatSourcePrimitive>(primitive)
                    .unwrap()
                    .model_matrix = model
            }
            3 => *world.get_mut::<GlobalTransform>(primitive).unwrap() = draws[0].1,
            _ => unreachable!(),
        }
        input.assert_bound(primitive);
        assert_full_oracle(&mut input);
    }
}

#[test]
fn semantic_r7_hidden_flat_edits_and_removals_recover_without_an_extra_hot_write() {
    for variant in 0..3 {
        let mut input = Fixture::pending_with_disablers(|world| {
            world.register_disabling_component::<SemanticR7Hidden>();
        });
        let primitive = input.spawn_stock();
        let global = flatten_stock(&mut input, primitive);
        let metadata = input
            .app
            .world()
            .get::<FlatSourcePrimitive>(primitive)
            .unwrap()
            .clone();
        input.assert_bound(primitive);
        input
            .app
            .world_mut()
            .entity_mut(primitive)
            .insert(SemanticR7Hidden);
        input.assert_blocked();
        match variant {
            0 => {
                input
                    .app
                    .world_mut()
                    .get_mut::<FlatSourcePrimitive>(primitive)
                    .unwrap()
                    .model_matrix = metadata.model_matrix * Mat4::from_translation(Vec3::X)
            }
            1 => {
                input
                    .app
                    .world_mut()
                    .entity_mut(primitive)
                    .remove::<GlobalTransform>();
            }
            2 => {
                input
                    .app
                    .world_mut()
                    .entity_mut(primitive)
                    .remove::<FlatSourcePrimitive>();
            }
            _ => unreachable!(),
        }
        // Consume the hidden mutation windows before removing only the disabler.
        input.assert_blocked();
        input.assert_blocked();
        let before = input
            .app
            .world()
            .entity(primitive)
            .get_change_ticks::<FlatSourcePrimitive>()
            .map(|ticks| ticks.changed);
        input
            .app
            .world_mut()
            .entity_mut(primitive)
            .remove::<SemanticR7Hidden>();
        let after = input
            .app
            .world()
            .entity(primitive)
            .get_change_ticks::<FlatSourcePrimitive>()
            .map(|ticks| ticks.changed);
        assert_eq!(before, after, "reenabling must not rewrite metadata");
        input.assert_blocked();
        assert_delta_only(&input);
        assert_full_oracle(&mut input);
        input
            .app
            .world_mut()
            .entity_mut(primitive)
            .insert((metadata, global));
        input.assert_bound(primitive);
        assert_full_oracle(&mut input);
    }
}

#[test]
fn semantic_settled_flat_other_fields_admit_without_affine_writes() {
    for variant in 0..4 {
        let (mut input, primitive, _) = observation_population(8);
        let original = input
            .app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let replacement = input
            .app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        input.app.world_mut().entity_mut(primitive).insert((
            MeshMaterial3d(original.clone()),
            RenderLayers::layer(3),
            Visibility::Inherited,
            InheritedVisibility::VISIBLE,
        ));
        input.assert_bound(primitive);
        input.assert_bound(primitive);
        let global_tick = input
            .app
            .world()
            .entity(primitive)
            .get_change_ticks::<GlobalTransform>()
            .unwrap()
            .changed;
        let flat_tick = input
            .app
            .world()
            .entity(primitive)
            .get_change_ticks::<FlatSourcePrimitive>()
            .unwrap()
            .changed;
        match variant {
            0 => {
                input
                    .app
                    .world_mut()
                    .get_mut::<MeshMaterial3d<StandardMaterial>>(primitive)
                    .unwrap()
                    .0 = replacement
            }
            1 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<RenderLayers>(primitive)
                    .unwrap() = RenderLayers::layer(5)
            }
            2 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<Visibility>(primitive)
                    .unwrap() = Visibility::Hidden
            }
            3 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<InheritedVisibility>(primitive)
                    .unwrap() = InheritedVisibility::HIDDEN
            }
            _ => unreachable!(),
        }
        assert_eq!(
            global_tick,
            input
                .app
                .world()
                .entity(primitive)
                .get_change_ticks::<GlobalTransform>()
                .unwrap()
                .changed
        );
        assert_eq!(
            flat_tick,
            input
                .app
                .world()
                .entity(primitive)
                .get_change_ticks::<FlatSourcePrimitive>()
                .unwrap()
                .changed
        );
        input.extract();
        let work = input.sources().last_work;
        assert_eq!(work.hot_flat_scans, 0);
        assert_eq!(work.hot_tuple_rows, 0);
        assert_eq!(work.hot_entities_yielded, 1);
        assert_eq!(work.hot_fact_change_reads, 1);
        assert_eq!(
            work.changed_entities, 1,
            "admit the flat edit before oracle recovery"
        );
        assert_delta_only(&input);
        assert_full_oracle(&mut input);
        match variant {
            0 => {
                input
                    .app
                    .world_mut()
                    .get_mut::<MeshMaterial3d<StandardMaterial>>(primitive)
                    .unwrap()
                    .0 = original
            }
            1 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<RenderLayers>(primitive)
                    .unwrap() = RenderLayers::layer(3)
            }
            2 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<Visibility>(primitive)
                    .unwrap() = Visibility::Inherited
            }
            3 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<InheritedVisibility>(primitive)
                    .unwrap() = InheritedVisibility::VISIBLE
            }
            _ => unreachable!(),
        }
        input.assert_bound(primitive);
        assert_eq!(input.sources().last_work.hot_flat_scans, 0);
        assert_eq!(input.sources().last_work.hot_fact_change_reads, 1);
        assert_full_oracle(&mut input);
    }
}

#[test]
fn semantic_negative_and_missing_snapshots_keep_distinct_publication_states() {
    let entity = World::new().spawn_empty().id();
    let mut cache = EntityHashMap::default();
    let mut work = SourceExtractionWork::default();
    cache.insert(
        entity,
        hot_facts(
            (None, None, None, None, None, None, None, None, None),
            false,
            IVec2::ZERO,
            &mut work,
        ),
    );
    let mut observed = HotObservations::default();
    assert!(observed.changed(entity, &cache, |_| None));
    assert!(matches!(observed.updates.get(&entity), Some(None)));
    assert!(!observed.changed(entity, &cache, |_| panic!("duplicate must not read facts")));
    let mut missing = HotObservations::default();
    assert!(
        missing.changed(entity, &EntityHashMap::default(), |_| panic!(
            "missing prior snapshot admits publication"
        ))
    );
    assert!(missing.seen.contains(&entity));
    assert!(!missing.updates.contains_key(&entity));
}
