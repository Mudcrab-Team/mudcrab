#[test]
fn readiness_source_deltas_include_fallback_only_and_accumulate_until_observed() {
    let mut changes = SourceReadinessChanges::default();
    changes.record(7, (false, false), (false, true));
    changes.record(19, (false, false), (true, false));
    changes.record(7, (false, true), (false, false));
    changes.record(5, (true, false), (true, false));
    assert_eq!(changes.batches, BTreeSet::from([7, 19]));
    assert_eq!(
        std::mem::take(&mut changes.batches),
        BTreeSet::from([7, 19])
    );
    assert!(changes.batches.is_empty());
}

#[test]
fn actual_template_producer_publishes_only_changed_loading_tuples() {
    let (mut input, material, _) = layout_batch();
    input
        .render
        .resource_mut::<Sources>()
        .readiness_changes
        .batches
        .clear();
    input
        .app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .get_mut(&material)
        .unwrap()
        .depth_map = Some(Handle::default());
    input
        .app
        .world_mut()
        .resource_mut::<Messages<AssetEvent<StandardMaterial>>>()
        .write(AssetEvent::Modified { id: material.id() });
    input.extract();
    assert!(input.sources().source_fallback_batches.contains(&0));
    assert_eq!(
        std::mem::take(
            &mut input
                .render
                .resource_mut::<Sources>()
                .readiness_changes
                .batches
        ),
        BTreeSet::from([0])
    );
    input.extract();
    assert!(input.sources().readiness_changes.batches.is_empty());
    input
        .app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .get_mut(&material)
        .unwrap()
        .depth_map = None;
    input
        .app
        .world_mut()
        .resource_mut::<Messages<AssetEvent<StandardMaterial>>>()
        .write(AssetEvent::Modified { id: material.id() });
    input.extract();
    assert!(!input.sources().source_fallback_batches.contains(&0));
    assert_eq!(
        input.sources().readiness_changes.batches,
        BTreeSet::from([0])
    );
}
