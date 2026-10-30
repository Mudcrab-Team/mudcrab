//! Retained loading contributions at consumer, batch and domain granularity.
use super::*;
use std::collections::BTreeSet;

type BatchFacts = (bool, bool, bool, bool);

#[derive(Default)]
pub(super) struct Aggregate {
    states: Vec<ConsumerState>,
    worlds: HashMap<u32, ClusterWorldReadiness>,
    initialized: bool,
    #[cfg(test)]
    pub(super) visits: usize,
}

struct ConsumerState {
    consumer: Consumer,
    rows: HashMap<(usize, bool), ClusterWorldReadiness>,
    total: ClusterWorldReadiness,
}

fn same_domain(a: &Consumer, b: &Consumer) -> bool {
    a.key == b.key && a.world == b.world && a.shadow == b.shadow && a.layers == b.layers
}

fn update_counts(
    total: &mut ClusterWorldReadiness,
    delta: ClusterWorldReadiness,
    update: impl Fn(usize, usize) -> usize,
) {
    total.applicable_cameras = update(total.applicable_cameras, delta.applicable_cameras);
    total.missing_views = update(total.missing_views, delta.missing_views);
    total.needed_batches = update(total.needed_batches, delta.needed_batches);
    total.pending_materials = update(total.pending_materials, delta.pending_materials);
    total.pending_templates = update(total.pending_templates, delta.pending_templates);
    total.pending_compute = update(total.pending_compute, delta.pending_compute);
    total.pending_pipelines = update(total.pending_pipelines, delta.pending_pipelines);
    total.failed_batches = update(total.failed_batches, delta.failed_batches);
}

fn contribution(
    consumer: &Consumer,
    need: &NeededBatch,
    facts: BatchFacts,
) -> ClusterWorldReadiness {
    let domain = ViewDomain::new(&consumer.layers, consumer.shadow);
    let (compute, template, failed, casts_shadows) = facts;
    if !(if need.terrain {
        domain.terrain
    } else {
        domain.objects
    }) || (consumer.shadow && !casts_shadows)
    {
        return ClusterWorldReadiness::default();
    }
    ClusterWorldReadiness {
        needed_batches: 1,
        pending_compute: usize::from(!compute),
        pending_templates: usize::from(!need.terrain && !template),
        failed_batches: usize::from(failed || need.invalid_owned_state),
        pending_pipelines: usize::from(
            !consumer
                .readiness
                .as_ref()
                .is_some_and(|ready| ready.get(need.batch) == Some(&true)),
        ),
        ..default()
    }
}

impl Aggregate {
    pub(super) fn observe(
        &mut self,
        consumers: Vec<Consumer>,
        catalog: &NeededCatalog,
        changed: &BTreeSet<usize>,
        facts: impl Fn(usize) -> BatchFacts,
        force: bool,
    ) -> Option<HashMap<u32, ClusterWorldReadiness>> {
        if self.initialized
            && changed.is_empty()
            && self.states.len() == consumers.len()
            && self
                .states
                .iter()
                .zip(&consumers)
                .all(|(a, b)| a.consumer.same(b))
        {
            if force {
                #[cfg(test)]
                crate::work_counters::record("cluster_drawability/republication");
            }
            return force.then(|| self.worlds.clone());
        }

        #[cfg(test)]
        crate::work_counters::record("cluster_drawability/non_replay");
        let mut old: Vec<_> = std::mem::take(&mut self.states)
            .into_iter()
            .map(Some)
            .collect();
        let mut prepared = HashMap::new();
        #[cfg(test)]
        let mut visits = 0;
        for consumer in consumers {
            let previous = old
                .iter_mut()
                .find(|state| {
                    state
                        .as_ref()
                        .is_some_and(|state| same_domain(&state.consumer, &consumer))
                })
                .and_then(Option::take);
            let mut state = if let Some(state) = previous {
                state
            } else {
                let mut state = ConsumerState {
                    consumer: consumer.clone(),
                    rows: HashMap::new(),
                    total: default(),
                };
                for need in catalog
                    .get(&consumer.world)
                    .into_iter()
                    .flat_map(|rows| rows.values())
                {
                    #[cfg(test)]
                    {
                        visits += 1;
                    }
                    let facts = *prepared
                        .entry(need.batch)
                        .or_insert_with(|| facts(need.batch));
                    let row = contribution(&consumer, need, facts);
                    update_counts(&mut state.total, row, |a, b| a + b);
                    state.rows.insert((need.batch, need.terrain), row);
                }
                state.total.applicable_cameras = usize::from(!consumer.shadow);
                state.total.missing_views = usize::from(consumer.readiness.is_none());
                self.states.push(state);
                continue;
            };
            let mut touched = changed.clone();
            let same_bits = match (&state.consumer.readiness, &consumer.readiness) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if !same_bits {
                let old = state
                    .consumer
                    .readiness
                    .as_deref()
                    .map_or(&[][..], |bits| bits.as_slice());
                let new = consumer
                    .readiness
                    .as_deref()
                    .map_or(&[][..], |bits| bits.as_slice());
                // A producer receipt avoids another vector walk. Missing receipt
                // continuity uses the complete bit comparison, never a count/hash.
                if let Some(delta) = consumer
                    .readiness_changes
                    .as_ref()
                    .filter(|delta| delta.matches(&state.consumer.readiness, &consumer.readiness))
                {
                    touched.extend(&delta.batches);
                } else {
                    #[cfg(test)]
                    crate::work_counters::record_count(
                        "cluster_drawability/readiness_comparison_bits",
                        old.len().max(new.len()) as u64,
                    );
                    touched.extend((0..old.len().max(new.len())).filter(|&batch| {
                        old.get(batch).copied().unwrap_or(false)
                            != new.get(batch).copied().unwrap_or(false)
                    }));
                }
            }
            state.consumer = consumer;
            state.total.applicable_cameras = usize::from(!state.consumer.shadow);
            state.total.missing_views = usize::from(state.consumer.readiness.is_none());
            for batch in touched {
                for terrain in [false, true] {
                    let key = (batch, terrain);
                    if let Some(row) = state.rows.remove(&key) {
                        update_counts(&mut state.total, row, |a, b| a - b);
                    }
                    if let Some(need) = catalog
                        .get(&state.consumer.world)
                        .and_then(|rows| rows.get(&key))
                    {
                        #[cfg(test)]
                        {
                            visits += 1;
                        }
                        let facts = *prepared.entry(batch).or_insert_with(|| facts(batch));
                        let row = contribution(&state.consumer, need, facts);
                        update_counts(&mut state.total, row, |a, b| a + b);
                        state.rows.insert(key, row);
                    }
                }
            }
            self.states.push(state);
        }
        let mut worlds = HashMap::new();
        for state in &self.states {
            update_counts(
                worlds.entry(state.consumer.world).or_default(),
                state.total,
                |a, b| a + b,
            );
        }
        #[cfg(test)]
        crate::work_counters::record_count("cluster_drawability/aggregate_batch_visits", visits);
        #[cfg(test)]
        crate::work_counters::record_count(
            "cluster_drawability/batch_fact_preparations",
            prepared.len() as u64,
        );
        #[cfg(test)]
        {
            self.visits += visits as usize;
        }
        let publish = !self.initialized || force || worlds != self.worlds;
        self.worlds = worlds;
        self.initialized = true;
        publish.then(|| self.worlds.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::consumer;
    use super::*;

    fn catalog() -> NeededCatalog {
        HashMap::from([(
            42,
            (0..100)
                .map(|batch| {
                    (
                        (batch, false),
                        NeededBatch {
                            batch,
                            terrain: false,
                            invalid_owned_state: false,
                        },
                    )
                })
                .collect(),
        )])
    }

    #[test]
    fn successive_deltas_match_fresh_world_domain_aggregation_before_any_oracle_repair() {
        let mut aggregate = Aggregate::default();
        let mut catalog = catalog();
        catalog.get_mut(&42).unwrap().insert(
            (0, true),
            NeededBatch {
                batch: 0,
                terrain: true,
                invalid_owned_state: false,
            },
        );
        catalog.insert(
            43,
            HashMap::from([(
                (0, false),
                NeededBatch {
                    batch: 0,
                    terrain: false,
                    invalid_owned_state: false,
                },
            )]),
        );
        let ready = Arc::new(vec![true; 100]);
        let mut consumers = vec![
            consumer(1, false, Some(ready.clone())),
            consumer(2, true, Some(ready)),
        ];
        let mut facts = vec![(true, true, false, true); 100];
        for step in 0..64 {
            let batch = (step * 17) % 100;
            let mut changed = BTreeSet::new();
            match step % 8 {
                0 => {
                    facts[batch].1 = !facts[batch].1;
                    changed.insert(batch);
                }
                1 => {
                    facts[batch].2 = !facts[batch].2;
                    changed.insert(batch);
                }
                2 => {
                    facts[batch].3 = !facts[batch].3;
                    changed.insert(batch);
                }
                3 => {
                    let old = consumers[0].readiness.as_ref().unwrap();
                    let mut bits = old.as_ref().clone();
                    bits[batch] = !bits[batch];
                    let current = Arc::new(bits);
                    consumers[0].readiness_changes =
                        Some(Arc::new(ReadinessChanges::between(Some(old), &current)));
                    consumers[0].readiness = Some(current);
                }
                4 => {
                    let row = catalog.get_mut(&42).unwrap().get_mut(&(0, false)).unwrap();
                    row.invalid_owned_state = !row.invalid_owned_state;
                    changed.insert(0);
                }
                5 => {
                    let rows = catalog.get_mut(&42).unwrap();
                    if rows.remove(&(99, false)).is_none() {
                        rows.insert(
                            (99, false),
                            NeededBatch {
                                batch: 99,
                                terrain: false,
                                invalid_owned_state: false,
                            },
                        );
                    }
                    changed.insert(99);
                }
                6 => {
                    consumers[1].world = if consumers[1].world == 42 { 43 } else { 42 };
                }
                _ => {
                    consumers[0].layers =
                        if consumers[0].layers == crate::render::PLACED_OBJECT_RENDER_LAYERS {
                            RenderLayers::layer(crate::render::WORLD_LAYER)
                        } else {
                            crate::render::PLACED_OBJECT_RENDER_LAYERS
                        };
                }
            }
            aggregate.observe(
                consumers.clone(),
                &catalog,
                &changed,
                |batch| facts[batch],
                false,
            );
            let mut oracle = Aggregate::default();
            let expected = oracle
                .observe(
                    consumers.clone(),
                    &catalog,
                    &BTreeSet::new(),
                    |batch| facts[batch],
                    false,
                )
                .unwrap();
            assert_eq!(aggregate.worlds, expected, "delta {step}");
            let visits = aggregate.visits;
            assert!(
                aggregate
                    .observe(
                        consumers.clone(),
                        &catalog,
                        &BTreeSet::new(),
                        |_| panic!("settled delta {step}"),
                        false
                    )
                    .is_none()
            );
            assert_eq!(aggregate.visits, visits);
        }
    }

    #[test]
    fn fallback_only_delta_replaces_one_batch_and_recovery_removes_its_failure() {
        let mut aggregate = Aggregate::default();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true; 100])))];
        let catalog = catalog();
        let initial = aggregate
            .observe(
                consumers.clone(),
                &catalog,
                &BTreeSet::new(),
                |_| (true, false, false, true),
                false,
            )
            .unwrap();
        assert_eq!(initial[&42].failed_batches, 0);
        aggregate.visits = 0;
        let calls = std::cell::Cell::new(0);
        let failed = aggregate
            .observe(
                consumers.clone(),
                &catalog,
                &BTreeSet::from([37]),
                |batch| {
                    calls.set(calls.get() + 1);
                    (true, false, batch == 37, true)
                },
                false,
            )
            .unwrap();
        assert_eq!(failed[&42].failed_batches, 1);
        assert_eq!(failed[&42].pending_templates, 100);
        assert_eq!(aggregate.visits, 1);
        assert_eq!(calls.get(), 1);
        let recovered = aggregate
            .observe(
                consumers,
                &catalog,
                &BTreeSet::from([37]),
                |_| (true, false, false, true),
                false,
            )
            .unwrap();
        assert_eq!(recovered[&42].failed_batches, 0);
        assert_eq!(aggregate.visits, 2);
    }

    #[test]
    fn generation_republication_uses_current_retained_proof_without_batch_work() {
        let mut aggregate = Aggregate::default();
        let consumers = vec![consumer(1, false, Some(Arc::new(vec![true; 100])))];
        let catalog = catalog();
        let first = aggregate
            .observe(
                consumers.clone(),
                &catalog,
                &BTreeSet::new(),
                |_| (true, true, false, true),
                false,
            )
            .unwrap();
        aggregate.visits = 0;
        let second = aggregate
            .observe(
                consumers,
                &catalog,
                &BTreeSet::new(),
                |_| panic!("unchanged dependencies"),
                true,
            )
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(aggregate.visits, 0);
    }

    #[test]
    fn one_consumer_bit_change_does_not_revisit_other_batches_or_views() {
        let mut aggregate = Aggregate::default();
        let ready = Arc::new(vec![true; 100]);
        let mut consumers = vec![
            consumer(1, false, Some(ready.clone())),
            consumer(2, true, Some(ready)),
        ];
        let catalog = catalog();
        aggregate
            .observe(
                consumers.clone(),
                &catalog,
                &BTreeSet::new(),
                |_| (true, true, false, true),
                false,
            )
            .unwrap();
        aggregate.visits = 0;
        let mut changed = vec![true; 100];
        changed[19] = false;
        consumers[0].readiness = Some(Arc::new(changed));
        let result = aggregate
            .observe(
                consumers,
                &catalog,
                &BTreeSet::new(),
                |_| (true, true, false, true),
                false,
            )
            .unwrap();
        assert_eq!(result[&42].pending_pipelines, 1);
        assert_eq!(aggregate.visits, 1);
    }
}
