mod settled_resource_writes {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    struct Sample {
        far_changed: bool,
        metrics_changed: bool,
        queued: usize,
        pending: usize,
        failed: usize,
    }

    #[derive(Resource, Default)]
    struct Samples(Vec<Sample>);

    fn observe_after_producers(
        far: Res<FarStock>,
        metrics: Res<StreamingMetrics>,
        mut samples: ResMut<Samples>,
    ) {
        samples.0.push(Sample {
            far_changed: far.is_changed(),
            metrics_changed: metrics.is_changed(),
            queued: metrics.queued_far_stock_queries,
            pending: metrics.pending_far_stock_queries,
            failed: metrics.failed_far_stock_sources,
        });
    }

    fn attach_observer(app: &mut App) {
        app.init_resource::<Samples>()
            .add_systems(Last, observe_after_producers);
        // A newly installed observer sees each resource's earlier insertion/change tick.
        // Discard only that first observation, then measure unchanged real frames.
        app.update();
        app.world_mut().resource_mut::<Samples>().0.clear();
    }

    fn plan_only_app(fixture: &Fixture, key: CellKey) -> App {
        let mut app = complete_plan_app(fixture, key);
        // Keep the exact existing resource/camera fixture, replacing only its Update
        // schedule so publish_work cannot masquerade as a plan resource writer.
        app.add_schedule(Schedule::new(Update))
            .add_systems(Update, plan);
        app
    }

    #[test]
    fn settled_populated_plan_does_not_publish_false_resource_changes() {
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        let mut app = plan_only_app(&fixture, key);
        let parent = root(app.world_mut(), key, false);
        app.world_mut()
            .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)));
        for _ in 0..4 {
            app.update();
        }
        assert!(!app.world().resource::<FarStock>().owners.is_empty());
        attach_observer(&mut app);
        for _ in 0..4 {
            app.update();
        }
        let samples = &app.world().resource::<Samples>().0;
        assert_eq!(samples.len(), 4);
        for sample in samples {
            assert!(
                !sample.far_changed,
                "settled plan must not mark FarStock changed: {sample:?}"
            );
            assert!(
                !sample.metrics_changed,
                "plan-only fixture has no StreamingMetrics producer: {sample:?}"
            );
        }
    }

    fn populated_publisher(_fixture: &Fixture, key: CellKey, row: &ReferenceRow) -> (App, Entity) {
        // Existing failure_app installs publish_work alone, with no planner/readiness writer.
        let mut app = failure_app();
        let parent = app.world_mut().spawn_empty().id();
        let entity = app
            .world_mut()
            .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)))
            .id();
        failed_owner(&mut app, key, row.form_id, entity, parent);
        for _ in 0..4 {
            app.update();
        }
        assert!(!app.world().resource::<FarStock>().owners.is_empty());
        assert!(app.world().resource::<FarStock>().failed.is_empty());
        attach_observer(&mut app);
        (app, entity)
    }

    #[test]
    fn settled_publish_work_does_not_mark_false_memtake_or_equal_metric_writes() {
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        let (mut app, _) = populated_publisher(&fixture, key, &row);
        for _ in 0..4 {
            app.update();
        }
        let samples = &app.world().resource::<Samples>().0;
        assert_eq!(samples.len(), 4);
        for sample in samples {
            assert_eq!((sample.queued, sample.pending, sample.failed), (0, 0, 0));
            assert!(
                !sample.far_changed,
                "taking an already-false failure flag must not publish FarStock change: {sample:?}"
            );
            assert!(
                !sample.metrics_changed,
                "equal publication must not publish StreamingMetrics change: {sample:?}"
            );
        }
    }

    #[test]
    fn publish_work_still_publishes_new_queue_and_real_failure_wakeups() {
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        let (mut app, entity) = populated_publisher(&fixture, key, &row);
        app.world_mut()
            .resource_mut::<FarStock>()
            .set_queued(VecDeque::from([(key, BTreeSet::from([row.form_id]))]));
        app.update();
        let sample = *app.world().resource::<Samples>().0.last().unwrap();
        assert!(
            sample.metrics_changed,
            "a new queue must publish its changed count"
        );
        assert_eq!((sample.queued, sample.pending, sample.failed), (1, 0, 0));
        assert_eq!(
            app.world_mut()
                .resource_mut::<FarStock>()
                .pop_queued()
                .unwrap()
                .0,
            key
        );
        app.update();
        let sample = *app.world().resource::<Samples>().0.last().unwrap();
        assert!(
            sample.metrics_changed,
            "draining the queue changes its published count"
        );
        assert_eq!((sample.queued, sample.pending, sample.failed), (0, 0, 0));

        // Actual AssetFailed insertion wakes the existing independent failure journal.
        app.world_mut().entity_mut(entity).insert(AssetFailed);
        app.update();
        let sample = *app.world().resource::<Samples>().0.last().unwrap();
        assert!(sample.far_changed);
        assert!(sample.metrics_changed);
        assert_eq!(sample.failed, 1);
        assert!(
            app.world().resource::<FarStock>().failed[&key]
                .contains(&format!("{:08X}", row.form_id))
        );
        assert!(
            app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
    }

    #[test]
    fn plan_still_wakes_for_reference_enable_and_rebuilds_exact_queue_values() {
        use crate::world::reference_state::{self, ReferenceEnableState, SetReferenceEnabled};
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        let identity = (cell_world(key).unwrap(), row.form_id);
        let mut app = plan_only_app(&fixture, key);
        reference_state::install(&mut app);
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(fixture.package.enable_catalog().clone());
        for _ in 0..4 {
            app.update();
        }
        assert!(
            app.world()
                .resource::<FarStock>()
                .desired
                .contains_key(&identity)
        );
        attach_observer(&mut app);
        for enabled in [false, true] {
            app.world_mut().trigger(SetReferenceEnabled {
                reference: row.form_id,
                enabled,
            });
            app.update();
            let sample = *app.world().resource::<Samples>().0.last().unwrap();
            assert!(
                sample.far_changed,
                "enable wake must update the actual planning resource"
            );
            let far = app.world().resource::<FarStock>();
            assert_eq!(far.desired.contains_key(&identity), enabled);
            assert_eq!(
                far.queued.iter().any(|(_, ids)| ids.contains(&row.form_id)),
                enabled
            );
            assert_eq!(
                app.world()
                    .resource::<ReferenceEnableState>()
                    .enabled(row.form_id),
                Some(enabled)
            );
        }
    }

    // Source delivery is the same public resource operation used by the actual
    // unchanged source_fallback_bridge::publish branch. This isolated headless
    // harness does not execute ExtractSchedule or claim renderer/GPU coverage.
    fn deliver_same_renderer_qualification(app: &mut App, fixture: &Fixture) {
        assert!(
            app.world_mut()
                .resource_mut::<SourceFallbackDemand>()
                .refresh_for_package(&fixture.package)
        );
    }

    fn accepted_reference(fixture: &Fixture) -> u32 {
        fixture
            .package
            .records()
            .next()
            .expect("producer fixture has an accepted record")
            .1
            .reference
    }

    fn fresh_feedback_plan(fixture: &Fixture, key: CellKey, row: &ReferenceRow) -> App {
        let mut app = plan_only_app(fixture, key);
        let parent = root(app.world_mut(), key, false);
        app.world_mut()
            .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)));
        let mut facts = crate::cluster::ownership::OwnershipFacts::default();
        facts.publish(
            &fixture.package,
            fixture
                .package
                .records()
                .filter(|(_, identity)| identity.reference == accepted_reference(fixture))
                .map(|(key, _)| (*key, false))
                .collect(),
        );
        assert!(facts.reference_has_owned(accepted_reference(fixture)));
        app.insert_resource(facts);
        app.world_mut()
            .resource_mut::<SourceFallbackDemand>()
            .update(Some(&fixture.package), BTreeSet::new());
        for _ in 0..4 {
            deliver_same_renderer_qualification(&mut app, fixture);
            app.update();
        }
        assert!(!app.world().resource::<FarStock>().owners.is_empty());
        assert!(!app.world().resource::<FarStock>().qualification_pending);
        app.init_resource::<Samples>()
            .add_systems(Last, observe_after_producers);
        deliver_same_renderer_qualification(&mut app, fixture);
        app.update();
        app.world_mut().resource_mut::<Samples>().0.clear();
        app
    }

    #[test]
    fn equal_fresh_renderer_deliveries_preserve_settled_far_stock_change_ticks() {
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        let mut app = fresh_feedback_plan(&fixture, key, &row);
        let revision = app.world().resource::<FarStock>().fallback_revision;
        for _ in 0..4 {
            deliver_same_renderer_qualification(&mut app, &fixture);
            app.update();
            assert!(!app.world().resource::<FarStock>().qualification_pending);
            assert_eq!(
                app.world().resource::<FarStock>().fallback_revision,
                revision
            );
            assert!(
                app.world()
                    .resource::<crate::cluster::ownership::OwnershipFacts>()
                    .reference_has_owned(accepted_reference(&fixture))
            );
            assert!(
                !app.world().resource::<SourceFallbackDemand>().has_fresh(),
                "the equal renderer delivery must still be acknowledged once"
            );
        }
        let samples = &app.world().resource::<Samples>().0;
        assert_eq!(samples.len(), 4);
        for sample in samples {
            assert!(
                !sample.far_changed,
                "equal fresh renderer delivery must not mark unchanged FarStock: {sample:?}"
            );
            assert!(
                !sample.metrics_changed,
                "isolated plan has no StreamingMetrics producer: {sample:?}"
            );
        }
    }

    #[test]
    fn missing_fresh_renderer_delivery_withdraws_qualification_then_settles_pending() {
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        let mut app = fresh_feedback_plan(&fixture, key, &row);
        app.update();
        assert!(
            app.world().resource::<FarStock>().qualification_pending,
            "retained demand is not fresh qualification when renderer delivery is absent"
        );
        assert!(app.world().resource::<FarStock>().cell_state(key).pending > 0);
        assert!(
            app.world()
                .resource::<Samples>()
                .0
                .last()
                .unwrap()
                .far_changed,
            "withdrawing valid qualification must publish the actual state change"
        );
        for _ in 0..3 {
            app.update();
            assert!(app.world().resource::<FarStock>().qualification_pending);
            assert!(
                !app.world()
                    .resource::<Samples>()
                    .0
                    .last()
                    .unwrap()
                    .far_changed,
                "already-pending missing delivery must settle without false writes"
            );
        }
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        deliver_same_renderer_qualification(&mut app, &fixture);
        app.update();
        assert!(!app.world().resource::<FarStock>().qualification_pending);
    }

    #[test]
    fn withdrawn_and_wrong_certificate_feedback_cannot_requalify_owned_sources() {
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        for wrong_certificate in [false, true] {
            let mut app = fresh_feedback_plan(&fixture, key, &row);
            let replacement = Arc::new(
                crate::cluster::overlay_fixture::open_fixture(fixture.directory.path(), false)
                    .unwrap(),
            );
            assert!(!Arc::ptr_eq(&replacement, &fixture.package));
            app.world_mut()
                .resource_mut::<SourceFallbackDemand>()
                .update(wrong_certificate.then_some(&replacement), BTreeSet::new());
            app.update();
            assert!(app.world().resource::<FarStock>().qualification_pending);
            assert!(app.world().resource::<FarStock>().cell_state(key).pending > 0);
            assert!(
                app.world()
                    .resource::<SourceFallbackDemand>()
                    .for_package(&fixture.package)
                    .is_none()
            );
            assert!(!app.world().resource::<SourceFallbackDemand>().has_fresh());
            assert!(
                app.world()
                    .resource::<crate::cluster::ownership::OwnershipFacts>()
                    .reference_has_owned(accepted_reference(&fixture)),
                "withdrawn feedback cannot revoke permanent source ownership"
            );
            app.world_mut()
                .resource_mut::<SourceFallbackDemand>()
                .update(Some(&fixture.package), BTreeSet::new());
            app.update();
            assert!(!app.world().resource::<FarStock>().qualification_pending);
            assert!(
                !app.world()
                    .contains_resource::<crate::streaming::failure::LoadingRefusal>()
            );
        }
    }

    #[test]
    fn new_terminal_ownership_refusal_still_wakes_while_equal_feedback_is_fresh() {
        let fixture = Fixture::new();
        let (key, row, _) = fixture.selected();
        let mut app = fresh_feedback_plan(&fixture, key, &row);
        let refused = fixture
            .package
            .records()
            .find(|(_, identity)| identity.reference != accepted_reference(&fixture))
            .map(|(_, identity)| identity.reference)
            .unwrap();
        let changes: Vec<_> = fixture
            .package
            .records()
            .filter(|(_, identity)| identity.reference == refused)
            .map(|(key, _)| (*key, Some(true)))
            .collect();
        app.world_mut()
            .resource_mut::<crate::cluster::ownership::OwnershipFacts>()
            .publish_updates(&fixture.package, false, changes);
        let expected_revision = app
            .world()
            .resource::<crate::cluster::ownership::OwnershipFacts>()
            .refusal_revision();
        deliver_same_renderer_qualification(&mut app, &fixture);
        app.update();
        let far = app.world().resource::<FarStock>();
        assert_eq!(far.refusal_revision, Some(expected_revision));
        assert!(far.permanent_refusals.contains(&refused));
        assert!(!far.qualification_pending);
        assert!(
            app.world()
                .resource::<Samples>()
                .0
                .last()
                .unwrap()
                .far_changed
        );
        assert!(
            app.world()
                .resource::<crate::cluster::ownership::OwnershipFacts>()
                .reference_has_owned(accepted_reference(&fixture))
        );
    }
}
