use bevy::ecs::{
    change_detection::Tick,
    component::Component,
    entity::Entity,
    prelude::{Changed, Query, Ref},
    query::QueryFilter,
    system::SystemParam,
};

/// Same-window conservative admission for a conjunctive native `Changed<T>` query.
///
/// `false` proves there are no changed rows in the probed structural scope;
/// `true` requests the original native query, which remains authoritative.
/// Removal and structural-change journals are still required by callers.
#[derive(Copy, Clone)]
pub(super) struct TickProbe {
    pub(super) changed: bool,
    #[cfg(test)]
    pub(super) rows_checked: u64,
}

#[cfg(test)]
impl TickProbe {
    /// Reuse this decision without counting the original scan twice.
    ///
    /// Valid only in the same tick window, without intervening writes, for an
    /// admitted conjunctive `Changed<T>` query for the same component `T` whose
    /// eligible rows are a subset of the probed scope.
    /// A positive superset decision may admit a quiet subset; its original
    /// native query must still select the actual entities.
    pub(super) fn reused(self) -> Self {
        Self {
            #[cfg(test)]
            rows_checked: 0,
            ..self
        }
    }
}

/// Owns the original conjunctive native query and its captured policy/window.
///
/// Admission borrows a native read-only lens from that same query. No second
/// query state, world-wide access, or persistent tick summary is needed.
/// Later disabling registration retains Bevy's existing captured-policy rules.
#[derive(SystemParam)]
pub(super) struct ComponentChangeQuery<'w, 's, T: Component, F: QueryFilter + 'static> {
    original: Query<'w, 's, Entity, (F, Changed<T>)>,
}

impl<T: Component, F: QueryFilter + 'static> ComponentChangeQuery<'_, '_, T, F> {
    pub(super) fn probe(&self) -> TickProbe {
        // Allow<T> is archetypal but lacks ArchetypeFilter in Bevy 0.19.
        // Native lens conversion retains this query's matched storage set,
        // captured policy, density, access and tick window while erasing the
        // filter type. Changed<T> is deliberately replaced by the exact tick
        // test below. Never erase an additional per-row predicate. The lens
        // clones query metadata per probe; measurements must include that cost.
        if !F::IS_ARCHETYPAL {
            return TickProbe {
                changed: true,
                #[cfg(test)]
                rows_checked: 0,
            };
        }
        let mut lens = self.original.as_readonly().transmute_lens_inner::<Ref<T>>();
        let structural = lens.query();
        let Ok(chunks) = structural.contiguous_iter() else {
            // Sparse T, sparse F, or a captured sparse default disabler makes
            // native contiguous iteration unavailable. Admit the original.
            return TickProbe {
                changed: true,
                #[cfg(test)]
                rows_checked: 0,
            };
        };
        let mut probe = TickProbe {
            changed: false,
            #[cfg(test)]
            rows_checked: 0,
        };
        for chunk in chunks {
            let Some((first, remainder)) = chunk.changed_ticks_slice().split_first() else {
                continue;
            };
            let last = chunk.last_run_tick();
            let now = chunk.this_run_tick();
            #[cfg(test)]
            {
                probe.rows_checked += 1;
            }
            if first.is_newer_than(last, now) {
                probe.changed = true;
                return probe;
            }

            // Native Changed is W > min(wrapping_age, MAX), with W <= MAX.
            // Folding the minimum age (initialized at MAX) is equivalent to
            // testing every remainder row, including wraparound and expiry.
            let minimum_age = remainder.iter().fold(Tick::MAX.get(), |minimum, tick| {
                minimum.min(now.get().wrapping_sub(tick.get()))
            });
            #[cfg(test)]
            {
                probe.rows_checked += remainder.len() as u64;
            }
            let window = now.get().wrapping_sub(last.get()).min(Tick::MAX.get());
            if window > minimum_age {
                probe.changed = true;
                return probe;
            }
        }
        probe
    }

    /// The unchanged native query remains authoritative after admission.
    pub(super) fn iter_changed(&self) -> impl Iterator<Item = Entity> + '_ {
        self.original.iter()
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use bevy::ecs::{
        entity_disabling::Disabled, prelude::*, query::QueryState, system::SystemState,
    };

    #[derive(Component, Clone, Copy, Debug, PartialEq)]
    struct Value(u64);
    #[derive(Component)]
    struct Hot;
    #[derive(Component)]
    struct Terrain;
    #[derive(Component)]
    struct Other;
    #[derive(Component, Default)]
    struct Hidden;
    #[derive(Component, Default)]
    #[component(storage = "SparseSet")]
    struct Sparse;

    type Consumer<T, F = ()> = SystemState<ComponentChangeQuery<'static, 'static, T, F>>;

    struct Observation {
        probe: TickProbe,
        native: Vec<Entity>,
    }

    fn sorted(mut entities: Vec<Entity>) -> Vec<Entity> {
        entities.sort_unstable();
        entities
    }

    // Compare entity sets, not just the admission bit: false must never lose
    // a native match, and a conservative true must not manufacture an entity.
    fn inspect<T: Component, F: QueryFilter + 'static>(
        query: &ComponentChangeQuery<'_, '_, T, F>,
    ) -> Observation {
        let probe = query.probe();
        let native = sorted(query.original.iter().collect());
        assert_eq!(sorted(query.iter_changed().collect()), native);
        let admitted = if probe.changed {
            sorted(query.iter_changed().collect())
        } else {
            Vec::new()
        };
        assert_eq!(
            admitted, native,
            "admission must preserve every native entity"
        );
        Observation { probe, native }
    }

    fn observe<T: Component, F: QueryFilter + 'static>(
        pair: &mut Consumer<T, F>,
        world: &World,
    ) -> Observation {
        inspect(&pair.get(world).unwrap())
    }

    fn assert_entities(observation: &Observation, expected: &[Entity]) {
        assert_eq!(observation.native, sorted(expected.to_vec()));
    }

    fn assert_quiet(observation: &Observation, rows: u64) {
        assert_entities(observation, &[]);
        assert!(!observation.probe.changed);
        assert_eq!(observation.probe.rows_checked, rows);
    }

    fn assert_fallback(observation: &Observation, expected: &[Entity]) {
        assert_entities(observation, expected);
        assert!(observation.probe.changed);
        assert_eq!(observation.probe.rows_checked, 0);
    }

    /// Persistent query states, with explicit windows only for boundary tests.
    /// Ordinary tests use persistent SystemState and Bevy's own window.
    struct WindowConsumer<T: Component, F: QueryFilter + 'static = ()> {
        original: QueryState<Entity, (F, Changed<T>)>,
    }

    impl<T: Component, F: QueryFilter + 'static> WindowConsumer<T, F> {
        fn new(world: &mut World) -> Self {
            Self {
                original: world.query_filtered(),
            }
        }

        fn at(&mut self, world: &World, last: Tick, now: Tick) -> Observation {
            // SAFETY: this state was initialized from this world and reads
            // only Entity and Changed/filter metadata. The shared world borrow
            // excludes all component/tick writers. The API validates identity
            // and refreshes archetypes. The probe lens inherits this exact
            // synthetic window from the original native query. Arbitrary ticks
            // affect the change predicate, not memory-access validity. This
            // test-only escape hatch reaches wrap/MAX without billions of runs.
            let query = unsafe {
                ComponentChangeQuery {
                    original: self.original.query_unchecked_with_ticks(
                        world.as_unsafe_world_cell_readonly(),
                        last,
                        now,
                    ),
                }
            };
            inspect(&query)
        }
    }

    #[test]
    fn empty_and_emptied_tables_are_quiet_in_a_persistent_consumer() {
        let mut world = World::new();
        let mut pair = Consumer::<Value>::new(&mut world);
        assert_quiet(&observe(&mut pair, &world), 0);
        let entity = world.spawn(Value(1)).id();
        assert_entities(&observe(&mut pair, &world), &[entity]);
        assert_quiet(&observe(&mut pair, &world), 1);
        world.despawn(entity);
        assert_quiet(&observe(&mut pair, &world), 0);
    }

    #[test]
    fn populated_quiet_scope_excludes_unmatched_and_default_disabled_rows() {
        let mut world = World::new();
        world.register_disabling_component::<Hidden>();
        let excluded: Vec<_> = (0..128).map(|i| world.spawn(Value(i)).id()).collect();
        let eligible: Vec<_> = (0..3).map(|i| world.spawn((Value(i), Hot)).id()).collect();
        let disabled = world.spawn((Value(10), Hot, Disabled)).id();
        let hidden = world.spawn((Value(11), Hot, Hidden)).id();
        let mut pair = Consumer::<Value, With<Hot>>::new(&mut world);
        assert_entities(&observe(&mut pair, &world), &eligible);
        assert_quiet(&observe(&mut pair, &world), 3);
        for entity in [excluded[0], disabled, hidden] {
            world.get_mut::<Value>(entity).unwrap().0 += 1;
        }
        assert_quiet(&observe(&mut pair, &world), 3);
        world.get_mut::<Value>(eligible[2]).unwrap().0 += 1;
        let result = observe(&mut pair, &world);
        assert_entities(&result, &[eligible[2]]);
        assert!(result.probe.changed);
        assert_eq!(
            result.probe.rows_checked, 3,
            "quiet first row forces the remainder"
        );
    }

    fn scope_then_quiet<F: QueryFilter + 'static>(world: &mut World, expected: &[Entity]) {
        let mut pair = Consumer::<Value, F>::new(world);
        assert_entities(&observe(&mut pair, world), expected);
        assert_quiet(&observe(&mut pair, world), expected.len() as u64);
    }

    #[test]
    fn native_dense_without_or_and_explicit_disabled_scopes_are_preserved() {
        let mut world = World::new();
        let hot = world.spawn((Value(1), Hot)).id();
        let terrain = world.spawn((Value(2), Terrain)).id();
        let plain = world.spawn(Value(3)).id();
        let disabled = world.spawn((Value(4), Hot, Disabled)).id();
        scope_then_quiet::<Without<Hot>>(&mut world, &[terrain, plain]);
        scope_then_quiet::<Or<(With<Hot>, With<Terrain>)>>(&mut world, &[hot, terrain]);
        // Mention Disabled structurally to opt into both sides of its policy.
        scope_then_quiet::<Or<(With<Disabled>, Without<Disabled>)>>(
            &mut world,
            &[hot, terrain, plain, disabled],
        );
        scope_then_quiet::<With<Disabled>>(&mut world, &[disabled]);
    }

    #[test]
    fn raw_equal_replacement_and_query_writes_follow_native_changed() {
        let mut world = World::new();
        let entity = world.spawn(Value(7)).id();
        let mut pair = Consumer::<Value>::new(&mut world);
        observe(&mut pair, &world);
        assert_quiet(&observe(&mut pair, &world), 1);

        world.get_mut::<Value>(entity).unwrap().0 = 8;
        assert_entities(&observe(&mut pair, &world), &[entity]);
        assert_quiet(&observe(&mut pair, &world), 1);

        world.entity_mut(entity).insert(Value(8));
        assert_entities(&observe(&mut pair, &world), &[entity]);
        assert_quiet(&observe(&mut pair, &world), 1);

        *world.get_mut::<Value>(entity).unwrap() = Value(8);
        assert_entities(&observe(&mut pair, &world), &[entity]);
        assert_quiet(&observe(&mut pair, &world), 1);

        let mut writer = world.query::<&mut Value>();
        for mut value in writer.iter_mut(&mut world) {
            value.0 += 1;
        }
        assert_entities(&observe(&mut pair, &world), &[entity]);
        assert_quiet(&observe(&mut pair, &world), 1);
    }

    #[test]
    fn shared_reads_mutable_fetch_and_bypass_do_not_report_changes() {
        let mut world = World::new();
        let entity = world.spawn(Value(7)).id();
        let mut pair = Consumer::<Value>::new(&mut world);
        observe(&mut pair, &world);
        let value = world.get_mut::<Value>(entity).unwrap();
        assert_eq!(value.0, 7);
        assert_eq!(world.get::<Value>(entity).unwrap().0, 7);
        for value in world.query::<&mut Value>().iter_mut(&mut world) {
            assert_eq!(value.0, 7);
        }
        assert_quiet(&observe(&mut pair, &world), 1);
        world
            .get_mut::<Value>(entity)
            .unwrap()
            .bypass_change_detection()
            .0 = 99;
        assert_eq!(world.get::<Value>(entity).unwrap().0, 99);
        assert_quiet(&observe(&mut pair, &world), 1);
    }

    #[test]
    fn first_positive_shortcuts_but_late_positive_counts_the_whole_remainder() {
        let mut world = World::new();
        let entities: Vec<_> = (0..33).map(|i| world.spawn(Value(i)).id()).collect();
        let mut pair = Consumer::<Value>::new(&mut world);
        let first = observe(&mut pair, &world);
        assert_entities(&first, &entities);
        assert_eq!(first.probe.rows_checked, 1);
        assert_quiet(&observe(&mut pair, &world), 33);
        world.get_mut::<Value>(entities[32]).unwrap().0 += 1;
        let late = observe(&mut pair, &world);
        assert_entities(&late, &[entities[32]]);
        assert_eq!(late.probe.rows_checked, 33);
        world.get_mut::<Value>(entities[0]).unwrap().0 += 1;
        let first = observe(&mut pair, &world);
        assert_entities(&first, &[entities[0]]);
        assert_eq!(first.probe.rows_checked, 1);
    }

    #[test]
    fn safe_tick_rollback_is_read_fresh_even_in_the_same_window() {
        let mut world = World::new();
        let first = world.spawn(Value(1)).id();
        let later = world.spawn(Value(2)).id();
        let mut pair = WindowConsumer::<Value>::new(&mut world);
        world
            .get_mut::<Value>(first)
            .unwrap()
            .set_last_changed(Tick::new(5));
        world
            .get_mut::<Value>(later)
            .unwrap()
            .set_last_changed(Tick::new(9));
        let (last, now) = (Tick::new(7), Tick::new(10));
        let positive = pair.at(&world, last, now);
        assert_entities(&positive, &[later]);
        assert_eq!(positive.probe.rows_checked, 2);
        world
            .get_mut::<Value>(later)
            .unwrap()
            .set_last_changed(Tick::new(5));
        assert_quiet(&pair.at(&world, last, now), 2);
        world
            .get_mut::<Value>(later)
            .unwrap()
            .set_last_changed(Tick::new(9));
        assert_entities(&pair.at(&world, last, now), &[later]);
    }

    #[test]
    fn remainder_matches_native_at_wrapping_and_max_age_boundaries() {
        let samples = [
            0,
            1,
            2,
            Tick::MAX.get() - 1,
            Tick::MAX.get(),
            Tick::MAX.get() + 1,
            u32::MAX - 1,
            u32::MAX,
        ];
        for len in [2, 7, 32, 33] {
            let mut world = World::new();
            let entities: Vec<_> = (0..len).map(|i| world.spawn(Value(i)).id()).collect();
            let mut pair = WindowConsumer::<Value>::new(&mut world);
            for now in samples {
                let oldest = Tick::new(now.wrapping_sub(Tick::MAX.get()));
                // Every row except the last is expired/quiet. The candidate
                // at the far end forces a complete fold, even for len > 32.
                for &entity in &entities {
                    world
                        .get_mut::<Value>(entity)
                        .unwrap()
                        .set_last_changed(oldest);
                }
                for changed in samples {
                    world
                        .get_mut::<Value>(*entities.last().unwrap())
                        .unwrap()
                        .set_last_changed(Tick::new(changed));
                    for last in samples {
                        let result = pair.at(&world, Tick::new(last), Tick::new(now));
                        let expected =
                            if Tick::new(changed).is_newer_than(Tick::new(last), Tick::new(now)) {
                                vec![*entities.last().unwrap()]
                            } else {
                                Vec::new()
                            };
                        assert_entities(&result, &expected);
                        assert_eq!(
                            result.probe.changed,
                            !expected.is_empty(),
                            "len={len} last={last} now={now} changed={changed}"
                        );
                        assert_eq!(
                            result.probe.rows_checked, len,
                            "quiet first row must force all remainder ticks"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn sparse_component_always_admits_the_unchanged_native_query() {
        let mut world = World::new();
        let mut pair = Consumer::<Sparse>::new(&mut world);
        assert_fallback(&observe(&mut pair, &world), &[]);
        let entity = world.spawn(Sparse).id();
        assert_fallback(&observe(&mut pair, &world), &[entity]);
        assert_fallback(&observe(&mut pair, &world), &[]);
        world.get_mut::<Sparse>(entity).unwrap().set_changed();
        assert_fallback(&observe(&mut pair, &world), &[entity]);
        world.despawn(entity);
        assert_fallback(&observe(&mut pair, &world), &[]);
    }

    fn sparse_scope<F: QueryFilter + 'static>(world: &mut World, expected: &[Entity]) {
        let mut pair = Consumer::<Value, F>::new(world);
        assert_fallback(&observe(&mut pair, world), expected);
        assert_fallback(&observe(&mut pair, world), &[]);
    }

    #[test]
    fn sparse_filters_in_a_shared_table_fall_back_without_losing_entities() {
        let mut world = World::new();
        let plain = world.spawn(Value(1)).id();
        let sparse = world.spawn((Value(2), Sparse)).id();
        assert_eq!(
            world.entity(plain).location().table_id,
            world.entity(sparse).location().table_id
        );
        sparse_scope::<With<Sparse>>(&mut world, &[sparse]);
        sparse_scope::<Without<Sparse>>(&mut world, &[plain]);
        sparse_scope::<Or<(With<Sparse>, With<Hot>)>>(&mut world, &[sparse]);
        let mut pair = Consumer::<Value, With<Sparse>>::new(&mut world);
        observe(&mut pair, &world);
        world.get_mut::<Value>(plain).unwrap().0 += 1;
        assert_fallback(&observe(&mut pair, &world), &[]);
        world.get_mut::<Value>(sparse).unwrap().0 += 1;
        assert_fallback(&observe(&mut pair, &world), &[sparse]);
    }

    #[test]
    fn sparse_default_disabler_forces_native_fallback_and_excludes_hidden_rows() {
        let mut world = World::new();
        world.register_disabling_component::<Sparse>();
        let plain = world.spawn(Value(1)).id();
        let hidden = world.spawn((Value(2), Sparse)).id();
        let mut pair = Consumer::<Value>::new(&mut world);
        assert_fallback(&observe(&mut pair, &world), &[plain]);
        assert_fallback(&observe(&mut pair, &world), &[]);
        world.get_mut::<Value>(hidden).unwrap().0 += 1;
        assert_fallback(&observe(&mut pair, &world), &[]);
        world.get_mut::<Value>(plain).unwrap().0 += 1;
        assert_fallback(&observe(&mut pair, &world), &[plain]);
    }

    fn late_registration<D: Component + Default>(new_consumer_is_sparse: bool) {
        let mut world = World::new();
        let plain = world.spawn(Value(1)).id();
        let hidden = world.spawn((Value(2), D::default())).id();
        // Both original and structural queries capture the old policy now.
        let mut old = Consumer::<Value>::new(&mut world);
        assert_entities(&observe(&mut old, &world), &[plain, hidden]);
        assert_quiet(&observe(&mut old, &world), 2);
        world.register_disabling_component::<D>();
        world.get_mut::<Value>(hidden).unwrap().0 += 1;
        let new_hidden = world.spawn((Value(3), D::default(), Other)).id();
        assert_entities(&observe(&mut old, &world), &[hidden, new_hidden]);
        // A brand-new table after registration still uses the old policy.
        assert_quiet(&observe(&mut old, &world), 3);
        let mut new = Consumer::<Value>::new(&mut world);
        let initial = observe(&mut new, &world);
        assert_entities(&initial, &[plain]);
        let quiet = observe(&mut new, &world);
        if new_consumer_is_sparse {
            assert_fallback(&quiet, &[]);
        } else {
            assert_quiet(&quiet, 1);
        }
        world.get_mut::<Value>(hidden).unwrap().0 += 1;
        assert_entities(&observe(&mut old, &world), &[hidden]);
        let hidden_only = observe(&mut new, &world);
        if new_consumer_is_sparse {
            assert_fallback(&hidden_only, &[]);
        } else {
            assert_quiet(&hidden_only, 1);
        }
    }

    #[test]
    fn late_dense_disabler_keeps_old_consumer_policy_and_new_consumer_captures_new_policy() {
        late_registration::<Hidden>(false);
    }

    #[test]
    fn late_sparse_disabler_keeps_old_consumer_dense_and_new_consumer_falls_back() {
        late_registration::<Sparse>(true);
    }

    #[test]
    fn new_tables_migrations_and_swap_removal_refresh_native_candidates() {
        let mut world = World::new();
        let entities: Vec<_> = (0..8).map(|i| world.spawn((Value(i), Hot)).id()).collect();
        let outside = world.spawn(Value(10)).id();
        let mut pair = Consumer::<Value, With<Hot>>::new(&mut world);
        assert_entities(&observe(&mut pair, &world), &entities);
        assert_quiet(&observe(&mut pair, &world), 8);
        world.despawn(entities[0]);
        world.entity_mut(entities[3]).insert(Other);
        assert_quiet(&observe(&mut pair, &world), 7);
        world.get_mut::<Value>(entities[7]).unwrap().0 += 1;
        assert_entities(&observe(&mut pair, &world), &[entities[7]]);
        // Removing the changed row must not leave its tick on a swapped row.
        world.get_mut::<Value>(entities[6]).unwrap().0 += 1;
        world.despawn(entities[6]);
        assert_quiet(&observe(&mut pair, &world), 6);
        world.get_mut::<Value>(outside).unwrap().0 += 1;
        world.entity_mut(outside).insert(Hot);
        assert_entities(&observe(&mut pair, &world), &[outside]);
        world.get_mut::<Value>(outside).unwrap().0 += 1;
        world.entity_mut(outside).remove::<Hot>();
        assert_quiet(&observe(&mut pair, &world), 6);
        let fresh = world.spawn((Value(20), Hot, Terrain)).id();
        assert_entities(&observe(&mut pair, &world), &[fresh]);
        assert_quiet(&observe(&mut pair, &world), 7);
        world.entity_mut(entities[3]).remove::<Other>();
        assert_quiet(&observe(&mut pair, &world), 7);
    }

    #[test]
    fn independent_consumers_retain_changes_until_each_observes_them() {
        let mut world = World::new();
        let entity = world.spawn(Value(1)).id();
        let mut fast = Consumer::<Value>::new(&mut world);
        let mut slow = Consumer::<Value>::new(&mut world);
        assert_entities(&observe(&mut fast, &world), &[entity]);
        assert_entities(&observe(&mut slow, &world), &[entity]);
        assert_quiet(&observe(&mut fast, &world), 1);
        assert_quiet(&observe(&mut slow, &world), 1);
        world.get_mut::<Value>(entity).unwrap().0 += 1;
        assert_entities(&observe(&mut fast, &world), &[entity]);
        assert_quiet(&observe(&mut fast, &world), 1);
        assert_entities(&observe(&mut slow, &world), &[entity]);
        assert_quiet(&observe(&mut slow, &world), 1);
        world.get_mut::<Value>(entity).unwrap().0 += 1;
        assert_entities(&observe(&mut slow, &world), &[entity]);
        assert_entities(&observe(&mut fast, &world), &[entity]);
    }

    #[test]
    fn same_window_superset_reuse_preserves_decision_and_zeros_only_rows() {
        let mut world = World::new();
        let hot = world.spawn((Value(1), Hot)).id();
        let terrain = world.spawn((Value(2), Hot, Terrain)).id();
        type Scopes = (
            ComponentChangeQuery<'static, 'static, Value, With<Hot>>,
            ComponentChangeQuery<'static, 'static, Value, (With<Hot>, With<Terrain>)>,
        );
        let mut pair = SystemState::<Scopes>::new(&mut world);
        {
            let (broad, narrow) = pair.get(&world).unwrap();
            assert_entities(&inspect(&broad), &[hot, terrain]);
            assert_entities(&inspect(&narrow), &[terrain]);
        }
        {
            let (broad, narrow) = pair.get(&world).unwrap();
            let probe = broad.probe();
            let reused = probe.reused();
            assert!(!probe.changed);
            assert_eq!(probe.rows_checked, 2);
            assert_eq!(reused.changed, probe.changed);
            assert_eq!(reused.rows_checked, 0);
            assert!(narrow.original.is_empty());
        }
        world.get_mut::<Value>(hot).unwrap().0 += 1;
        let (broad, narrow) = pair.get(&world).unwrap();
        let probe = broad.probe();
        let copied = probe; // Copy, while reused(self) consumes its own copy.
        let reused = probe.reused();
        assert!(copied.changed);
        assert_eq!(reused.changed, copied.changed);
        assert_eq!(reused.rows_checked, 0);
        assert!(copied.rows_checked > 0);
        assert!(
            narrow.original.is_empty(),
            "a positive superset may admit a quiet subset"
        );
        let admitted = if reused.changed {
            narrow.iter_changed().collect()
        } else {
            Vec::new()
        };
        assert_eq!(sorted(admitted), sorted(narrow.original.iter().collect()));
    }

    #[test]
    fn native_allow_of_sparse_presence_preserves_dense_admission() {
        let mut world = World::new();
        let plain = world.spawn(Value(1)).id();
        let sparse = world.spawn((Value(2), Sparse)).id();
        let mut pair = Consumer::<Value, bevy::ecs::query::Allow<Sparse>>::new(&mut world);
        assert_entities(&observe(&mut pair, &world), &[plain, sparse]);
        assert_quiet(&observe(&mut pair, &world), 2);
        world.get_mut::<Value>(sparse).unwrap().0 += 1;
        assert_entities(&observe(&mut pair, &world), &[sparse]);
        assert_quiet(&observe(&mut pair, &world), 2);
    }

    #[test]
    fn per_row_filter_falls_back_before_native_lens_erasure() {
        let mut world = World::new();
        world.spawn(Value(1));
        let subject = world.spawn((Value(2), Other)).id();
        let mut pair = Consumer::<Value, Changed<Other>>::new(&mut world);
        assert_fallback(&observe(&mut pair, &world), &[subject]);
        assert_fallback(&observe(&mut pair, &world), &[]);
        world.get_mut::<Value>(subject).unwrap().0 += 1;
        assert_fallback(&observe(&mut pair, &world), &[]);
        world.get_mut::<Value>(subject).unwrap().0 += 1;
        world.get_mut::<Other>(subject).unwrap().set_changed();
        assert_fallback(&observe(&mut pair, &world), &[subject]);
    }
}
