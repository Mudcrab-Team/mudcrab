//! Sparse IO admission, exact publication, priority, and applied-time regressions.

use super::*;
use crate::world::database::DatabaseRequest;

#[derive(Resource, Default)]
struct Publications {
    references: Vec<u32>,
    exhaust_on_apply: bool,
}

fn observe_publication(
    event: On<Add, FormId>,
    forms: Query<&FormId>,
    mut publications: ResMut<Publications>,
    mut budget: ResMut<StreamingCommitBudget>,
) {
    publications
        .references
        .push(forms.get(event.entity).unwrap().0);
    if publications.exhaust_on_apply {
        // Deterministic stand-in for elapsed application cost. The observer
        // runs when commands apply, not while reference commands are queued.
        budget.frame_started = Instant::now() - Duration::from_secs(1);
    }
}

fn distinct_rows(fixture: &Fixture) -> Vec<(CellKey, ReferenceRow)> {
    let (selected, _, _) = fixture.selected();
    let mut rows: Vec<_> = fixture
        .rows
        .iter()
        .filter(|(key, row)| {
            cell_world(*key) == cell_world(selected)
                && row.initially_enabled
                && row.model_path.is_some()
        })
        .cloned()
        .collect();
    rows.sort_by_key(|(key, row)| (grid(*key).x, grid(*key).y, row.form_id));
    rows.dedup_by_key(|(key, _)| *key);
    assert!(
        rows.len() >= 2,
        "producer fixture supplies two position grids"
    );
    rows.truncate(2);
    rows
}

fn response(key: CellKey, row: &ReferenceRow) -> ReferenceResponse {
    ReferenceResponse {
        generation: 0,
        key,
        result: Ok(vec![row.clone()]),
        query_micros: 0,
        queue_wait_micros: 0,
        total_request_micros: 0,
    }
}

fn ready_pending(key: CellKey, row: &ReferenceRow) -> Pending {
    let (_sender, receiver) = crossbeam_channel::bounded(1);
    Pending {
        ids: BTreeSet::from([row.form_id]),
        receiver,
        ready: Some(response(key, row)),
    }
}

fn pipeline_app(fixture: &Fixture, rows: &[(CellKey, ReferenceRow)]) -> App {
    let (key, row) = &rows[0];
    let mut app = layer_app(fixture, *key, row);
    app.init_resource::<Publications>()
        .init_resource::<crate::streaming::StreamingMetrics>()
        .add_observer(observe_publication)
        .add_systems(Update, publish_work.after(collect));
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .max_commit_micros_per_frame = u64::MAX;
    let mut far = app.world_mut().resource_mut::<FarStock>();
    far.desired = rows
        .iter()
        .map(|(key, row)| ((cell_world(*key).unwrap(), row.form_id), *key))
        .collect();
    far.pending = rows
        .iter()
        .map(|(key, row)| (*key, ready_pending(*key, row)))
        .collect();
    app
}

#[test]
fn sparse_pipeline_admission_uses_worker_backpressure_not_full_cell_count() {
    let fixture = Fixture::new();
    let rows = distinct_rows(&fixture);
    let (base_key, row) = &rows[0];
    let mut app = pipeline_app(&fixture, &rows[..1]);
    let (database, requests, _responses) = WorldDatabase::channel_fixture(2);
    app.insert_resource(database);
    app.world_mut()
        .resource_mut::<StreamingCommitBudget>()
        .remaining = 0;
    assert_eq!(
        app.world()
            .resource::<EngineConfig>()
            .max_cell_commits_per_frame,
        1
    );
    let wanted: Vec<_> = (0..3)
        .map(|offset| {
            let key = exterior(
                cell_world(*base_key).unwrap(),
                grid(*base_key) + IVec2::X * offset,
            );
            (
                key,
                BTreeSet::from([row.form_id.wrapping_add(offset as u32)]),
            )
        })
        .collect();
    {
        let mut far = app.world_mut().resource_mut::<FarStock>();
        far.pending.clear();
        for (key, ids) in &wanted {
            for &id in ids {
                far.set_target((cell_world(*key).unwrap(), id), Some(*key));
            }
        }
        far.set_queued(wanted.clone().into());
    }
    app.update();
    assert_eq!(requests.len(), 2);
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.pending.len(), 2);
    assert_eq!(far.queued, VecDeque::from([wanted[2].clone()]));
    assert!(far.submission_failure.is_some());
    assert_bookkeeping(far);

    app.update(); // A saturated worker must not lose or duplicate queued demand.
    assert_eq!(requests.len(), 2);
    assert_eq!(app.world().resource::<FarStock>().pending.len(), 2);
    let first = requests.try_recv().unwrap();
    match &first {
        DatabaseRequest::LoadReferences {
            key, reference_ids, ..
        } => {
            assert_eq!((*key, reference_ids), (wanted[0].0, &wanted[0].1));
        }
        _ => panic!("sparse admission emitted a full-cell request"),
    }
    app.update(); // One freed native queue slot admits the retained third request.
    let far = app.world().resource::<FarStock>();
    assert!(far.queued.is_empty());
    assert_eq!(far.pending.len(), 3);
    assert!(far.submission_failure.is_none());
    assert_eq!(requests.len(), 2);
    assert_bookkeeping(far);
    assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 0);
    drop(first); // Keep its response sender alive until after the final poll.
}

#[test]
fn sparse_pipeline_publication_preserves_full_cell_slots_and_exact_placement() {
    let fixture = Fixture::new();
    let rows = distinct_rows(&fixture);
    let mut app = pipeline_app(&fixture, &rows);
    app.world_mut()
        .resource_mut::<StreamingCommitBudget>()
        .remaining = 1;
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.owners.len(), 2);
    assert!(far.pending.is_empty());
    assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 1);
    assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 2);
    for (key, row) in &rows {
        let owner = &far.owners[&(cell_world(*key).unwrap(), row.form_id)];
        let pose = fixture.package.placed_reference(row.form_id).unwrap().pose;
        if row.bounds_valid {
            assert_placement(app.world(), owner.entity, pose, *key);
        } else {
            assert_placement_transform(app.world(), owner.entity, pose, *key);
            assert!(
                app.world()
                    .get::<ExpectedModelBounds>(owner.entity)
                    .is_none()
            );
            assert!(app.world().get::<InstanceBounds>(owner.entity).is_none());
        }
    }
    assert_no_exit(app.world());
}

#[test]
fn sparse_pipeline_exhausted_time_still_polls_and_retains_all_ready_responses() {
    let fixture = Fixture::new();
    let rows = distinct_rows(&fixture);
    let mut app = pipeline_app(&fixture, &rows);
    let mut senders = Vec::new();
    {
        let mut far = app.world_mut().resource_mut::<FarStock>();
        for (key, row) in &rows {
            let (sender, receiver) = crossbeam_channel::bounded(1);
            sender.try_send(response(*key, row)).unwrap();
            far.pending.insert(
                *key,
                Pending {
                    ids: BTreeSet::from([row.form_id]),
                    receiver,
                    ready: None,
                },
            );
            senders.push(sender);
        }
    }
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .max_commit_micros_per_frame = 1_000;
    {
        let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
        budget.remaining = 1;
        budget.commits = 1;
        budget.frame_started = Instant::now() - Duration::from_secs(1);
    }
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(far.owners.is_empty());
    assert_eq!(far.pending.len(), 2);
    assert!(far.pending.values().all(|pending| pending.ready.is_some()));
    assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 1);
    assert!(app.world().resource::<Publications>().references.is_empty());
    assert_bookkeeping(far);
    drop(senders);
}

#[test]
fn sparse_pipeline_checks_application_cost_before_next_publication() {
    let fixture = Fixture::new();
    let rows = distinct_rows(&fixture);
    let mut app = pipeline_app(&fixture, &rows);
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .max_commit_micros_per_frame = 1_000;
    app.world_mut()
        .resource_mut::<Publications>()
        .exhaust_on_apply = true;
    {
        let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
        budget.remaining = 2;
        budget.commits = 0;
        budget.frame_started = Instant::now();
    }
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(app.world().resource::<Publications>().references.len(), 1);
    assert_eq!(far.owners.len(), 1);
    assert_eq!(far.pending.len(), 1);
    assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 2);
    assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
    assert_no_exit(app.world());
}

#[test]
fn sparse_pipeline_ready_priority_is_deterministic_and_bypasses_slow_requests() {
    let fixture = Fixture::new();
    let rows = distinct_rows(&fixture);
    let sum = grid(rows[0].0) + grid(rows[1].0);
    let tie_center = IVec2::new(sum.x.div_euclid(2), sum.y.div_euclid(2));
    assert_eq!(
        (grid(rows[0].0) - tie_center).abs().max_element(),
        (grid(rows[1].0) - tie_center).abs().max_element(),
        "the real fixture supplies an equal-distance priority case"
    );
    for (center, expected) in [
        // Nearest order deliberately disagrees with lexicographic grid order.
        (grid(rows[1].0), vec![rows[1].1.form_id, rows[0].1.form_id]),
        (tie_center, vec![rows[0].1.form_id, rows[1].1.form_id]),
    ] {
        for reverse in [false, true] {
            let mut app = pipeline_app(&fixture, &rows);
            let slow_grid = if rows.iter().any(|(key, _)| grid(*key) == center) {
                center - IVec2::ONE
            } else {
                // In the tie case the closest request is not ready at all.
                center
            };
            let slow_key = exterior(cell_world(rows[0].0).unwrap(), slow_grid);
            assert!(rows.iter().all(|(key, _)| *key != slow_key));
            let (slow_sender, slow_receiver) = crossbeam_channel::bounded(1);
            {
                let mut far = app.world_mut().resource_mut::<FarStock>();
                far.demand.as_mut().unwrap().center = center;
                far.pending.clear();
                let mut ordered = rows.clone();
                if reverse {
                    ordered.reverse();
                }
                for (key, row) in &ordered {
                    far.pending.insert(*key, ready_pending(*key, row));
                }
                far.pending.insert(
                    slow_key,
                    Pending {
                        ids: BTreeSet::new(),
                        receiver: slow_receiver,
                        ready: None,
                    },
                );
            }
            app.update();
            assert_eq!(app.world().resource::<Publications>().references, expected);
            let far = app.world().resource::<FarStock>();
            assert_eq!(far.pending.len(), 1);
            assert!(far.pending[&slow_key].ready.is_none());
            assert_eq!(far.owners.len(), 2);
            assert_no_exit(app.world());
            drop(slow_sender);
        }
    }
}

#[test]
fn sparse_pipeline_valid_first_corrupt_later_validates_whole_response_before_publication() {
    let fixture = Fixture::new();
    let mut grouped = HashMap::<CellKey, Vec<ReferenceRow>>::new();
    for (key, row) in &fixture.rows {
        if row.initially_enabled && row.model_path.is_some() && row_matches(&fixture.package, row) {
            grouped.entry(*key).or_default().push(row.clone());
        }
    }
    let (key, mut same_cell) = grouped
        .into_iter()
        .find(|(_, rows)| rows.len() >= 2)
        .expect("real producer inventory supplies two valid rows in one position grid");
    same_cell.sort_by_key(|row| row.form_id);
    same_cell.truncate(2);
    let rows = vec![(key, same_cell[0].clone()), (key, same_cell[1].clone())];
    let mut app = pipeline_app(&fixture, &rows);
    assert!(
        same_cell
            .iter()
            .all(|row| row_matches(&fixture.package, row))
    );
    let mut corrupted = same_cell[1].clone();
    corrupted.position[0] += 1.0;
    assert!(!row_matches(&fixture.package, &corrupted));
    let mut pending = ready_pending(key, &same_cell[0]);
    pending.ids.insert(same_cell[1].form_id);
    pending.ready.as_mut().unwrap().result = Ok(vec![same_cell[0].clone(), corrupted]);
    app.world_mut()
        .resource_mut::<FarStock>()
        .pending
        .insert(key, pending);
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(app.world().resource::<Publications>().references.is_empty());
    assert!(far.owners.is_empty());
    assert!(far.holders.is_empty());
    assert!(far.pending.is_empty());
    assert!(far.failed.contains_key(&key));
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .failed_far_stock_sources,
        1
    );
    assert_bookkeeping(far);
}

#[test]
fn sparse_pipeline_failures_spend_expired_shared_time_before_later_failure_or_valid_work() {
    let fixture = Fixture::new();
    let rows = distinct_rows(&fixture);
    let (valid_key, row) = &rows[0];
    let mut app = pipeline_app(&fixture, &rows[..1]);
    let center = grid(*valid_key) + IVec2::X * 3;
    let failures = [
        exterior(cell_world(*valid_key).unwrap(), center),
        exterior(cell_world(*valid_key).unwrap(), center + IVec2::X),
    ];
    {
        let mut far = app.world_mut().resource_mut::<FarStock>();
        far.demand.as_mut().unwrap().center = center;
        for (ordinal, key) in failures.iter().enumerate() {
            let mut pending = ready_pending(*key, row);
            pending.ready.as_mut().unwrap().result =
                Err(format!("invented query failure {ordinal}"));
            far.pending.insert(*key, pending);
        }
    }
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .max_commit_micros_per_frame = 1_000;
    for pass in 0..3 {
        {
            let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
            budget.remaining = 0;
            budget.commits = 0;
            // Progress admits one atomic attempt. That attempt must spend work
            // even on failure, so the next attempt sees an exhausted frame.
            budget.frame_started = Instant::now() - Duration::from_secs(1);
        }
        app.update();
        let far = app.world().resource::<FarStock>();
        assert_eq!(far.pending.len(), 2 - pass);
        assert_eq!(far.failed.len(), (pass + 1).min(2));
        for key in failures.iter().take((pass + 1).min(2)) {
            assert!(far.failed.contains_key(key));
        }
        assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 0);
        assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
        assert_eq!(
            app.world().resource::<Publications>().references,
            if pass < 2 {
                Vec::new()
            } else {
                vec![row.form_id]
            }
        );
        if pass < 2 {
            assert!(far.pending.contains_key(valid_key));
            assert!(far.owners.is_empty());
        } else {
            assert_eq!(far.owners.len(), 1);
        }
        assert_eq!(
            app.world()
                .resource::<crate::streaming::StreamingMetrics>()
                .failed_far_stock_sources,
            (pass + 1).min(2)
        );
        assert_bookkeeping(far);
    }
}

#[test]
fn sparse_pipeline_exactness_failures_publish_nothing_and_count_shared_work() {
    let fixture = Fixture::new();
    let rows = distinct_rows(&fixture);
    let (key, row) = &rows[0];
    for case in 0..6 {
        let mut app = pipeline_app(&fixture, &rows[..1]);
        let mut bad = response(*key, row);
        match case {
            0 => bad.generation += 1,
            1 => bad.key = rows[1].0,
            2 => bad.result = Ok(Vec::new()),
            3 => bad.result = Ok(vec![row.clone(), row.clone()]),
            4 => {
                let mut edited = row.clone();
                edited.position[0] += 1.0;
                bad.result = Ok(vec![edited]);
            }
            _ => bad.result = Err("invented sparse query failure".to_owned()),
        }
        app.world_mut()
            .resource_mut::<FarStock>()
            .pending
            .get_mut(key)
            .unwrap()
            .ready = Some(bad);
        app.world_mut()
            .resource_mut::<StreamingCommitBudget>()
            .remaining = 0;
        app.update();
        let far = app.world().resource::<FarStock>();
        assert!(far.owners.is_empty(), "invalid response case {case}");
        assert!(far.holders.is_empty(), "invalid response case {case}");
        assert!(far.pending.is_empty(), "invalid response case {case}");
        assert!(far.failed.contains_key(key), "invalid response case {case}");
        assert!(app.world().resource::<Publications>().references.is_empty());
        assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 0);
        assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
        assert_eq!(
            app.world()
                .resource::<crate::streaming::StreamingMetrics>()
                .failed_far_stock_sources,
            1
        );
        assert_bookkeeping(far);
    }
}
