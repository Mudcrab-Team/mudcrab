// Deterministic extraction work; these explicit writes model the flat producer's
// actual/expected affine updates. They do not certify producer provenance.

fn observation_population(count: usize) -> (Fixture, Entity, Vec<(Entity, GlobalTransform)>) {
    assert!(count > 0);
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    let global = flatten_stock(&mut input, primitive);
    let mut draws = vec![(primitive, global)];
    let mesh = input.app.world().get::<Mesh3d>(primitive).unwrap().clone();
    let owner = input
        .app
        .world_mut()
        .spawn((FormId(u32::MAX), Transform::IDENTITY, ChildOf(input.cell)))
        .id();
    for _ in 1..count {
        let global = GlobalTransform::IDENTITY;
        let entity = input
            .app
            .world_mut()
            .spawn((
                mesh.clone(),
                FlatSourceOwner(owner),
                FlatSourcePrimitive {
                    model_matrix: Mat4::IDENTITY,
                    expected_global: global,
                },
                global,
            ))
            .remove::<Transform>()
            .id();
        draws.push((entity, global));
    }
    input.assert_bound(primitive);
    input.assert_bound(primitive);
    assert_eq!(input.sources().last_work.hot_fact_reads, 0);
    (input, primitive, draws)
}

fn observation_rebase(input: &mut Fixture, draws: &mut [(Entity, GlobalTransform)], origin: IVec2) {
    let previous = input.sources().origin;
    input.app.insert_resource(RenderOrigin(origin));
    input
        .app
        .world_mut()
        .get_mut::<Transform>(input.cell)
        .unwrap()
        .translation = crate::streaming::grid_translation(IVec2::ZERO, origin);
    let shift = crate::streaming::grid_translation(previous, origin);
    for (entity, global) in draws {
        *global = GlobalTransform::from(Mat4::from_translation(shift) * global.to_matrix());
        *input
            .app
            .world_mut()
            .get_mut::<GlobalTransform>(*entity)
            .unwrap() = *global;
        input
            .app
            .world_mut()
            .get_mut::<FlatSourcePrimitive>(*entity)
            .unwrap()
            .expected_global = *global;
    }
}

#[test]
fn crossing_observation_counts_flat_work_before_oracle() {
    let count = 1024;
    let (mut input, primitive, mut draws) = observation_population(count);
    observation_rebase(&mut input, &mut draws, IVec2::new(2, -3));
    input.assert_bound(primitive);
    let work = input.sources().last_work;
    assert_eq!(work.hot_entities_yielded, count as u64 + 1);
    assert_eq!(
        work.hot_fact_reads,
        work.hot_fact_change_reads + work.hot_fact_origin_reads + work.hot_fact_cache_reads
    );
    assert!(!work.qualification_full);
    assert_eq!(
        (
            work.changed_entities,
            work.journal_events,
            work.subjects_updated,
            work.ancestry_edges
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        (
            work.roots_scanned,
            work.meshes_scanned,
            work.primitives_scanned
        ),
        (0, 0, 0)
    );
    assert_full_oracle(&mut input);
}

#[test]
fn crossing_observation_rebase_ticks_do_not_certify_unchanged_facts() {
    let (mut input, primitive, mut draws) = observation_population(16);
    observation_rebase(&mut input, &mut draws, IVec2::X);
    let rebase_tick = input
        .app
        .world()
        .entity(primitive)
        .get_change_ticks::<GlobalTransform>()
        .unwrap()
        .changed;
    let bad = GlobalTransform::from_translation(Vec3::X) * draws[0].1;
    *input
        .app
        .world_mut()
        .get_mut::<GlobalTransform>(primitive)
        .unwrap() = bad;
    let authored_tick = input
        .app
        .world()
        .entity(primitive)
        .get_change_ticks::<GlobalTransform>()
        .unwrap()
        .changed;
    assert_eq!(
        rebase_tick, authored_tick,
        "ordinary World writes can share the same native tick"
    );
    input.assert_blocked();
    assert_eq!(input.sources().last_work.changed_entities, 1);
    assert_full_oracle(&mut input);
}

#[test]
fn crossing_observation_deduplicates_native_and_origin_fact_reads() {
    let count = 1024;
    let (mut input, primitive, mut draws) = observation_population(count);
    observation_rebase(&mut input, &mut draws, IVec2::new(-4, 7));
    input.assert_bound(primitive);
    let work = input.sources().last_work;
    assert_eq!(
        work.hot_fact_change_reads,
        count as u64 + 1,
        "read each admitted entity once"
    );
    assert_eq!(
        work.hot_fact_origin_reads, 0,
        "the changed cell already has a current snapshot"
    );
    assert_eq!(work.hot_fact_cache_reads, 0);
    assert_full_oracle(&mut input);
}

#[test]
fn crossing_observation_one_authored_edit_reads_one_hot_fact_snapshot() {
    let (mut input, primitive, draws) = observation_population(16);
    *input
        .app
        .world_mut()
        .get_mut::<GlobalTransform>(primitive)
        .unwrap() = GlobalTransform::from_translation(Vec3::X) * draws[0].1;
    input.assert_blocked();
    let work = input.sources().last_work;
    assert_eq!(work.hot_entities_yielded, 1);
    assert_eq!(
        work.hot_fact_reads, 1,
        "cache maintenance must reuse current observations"
    );
    assert_eq!(work.hot_fact_change_reads, 1);
    assert_eq!(work.hot_fact_cache_reads, 0);
    assert_full_oracle(&mut input);
}
