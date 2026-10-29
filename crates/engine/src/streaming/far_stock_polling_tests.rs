//! Distinguish journal probes from semantic projection and index mutation work.

use super::*;

fn counters(app: &App) -> near::PollingWork {
    app.world().resource::<FarStock>().near_sources.polling_work
}

fn count(work: &near::PollingWork, name: &str) -> u64 {
    match name {
        "far_stock/near_poll_frames" => work.poll_frames,
        "far_stock/near_reference_candidate_rows_upper" => work.reference_candidates_upper,
        "far_stock/near_reference_changed_yields" => work.reference_changed_yields,
        "far_stock/near_root_candidate_rows_upper" => work.root_candidates_upper,
        "far_stock/near_root_changed_yields" => work.root_changed_yields,
        "far_stock/near_removal_stream_polls" => work.removal_stream_polls,
        "far_stock/near_removed_events" => work.removed_events,
        "far_stock/near_index_inserts" => work.index_inserts,
        "far_stock/near_index_replacements" => work.index_replacements,
        "far_stock/near_index_removals" => work.index_removals,
        _ => panic!("unknown test work counter {name}"),
    }
}

#[test]
fn settled_journal_reads_no_population_candidates_without_projecting_values() {
    let fixture = Fixture::new();
    let key = fixture.selected().0;
    let mut app = complete_plan_app(&fixture, key);
    let parent = root(app.world_mut(), key, false);
    for id in 0..32 {
        // Not certified: only their lifecycle events and initial baseline inspect these rows.
        app.world_mut()
            .spawn((FormId(u32::MAX - id), CellRef(0), ChildOf(parent)));
    }
    app.update();
    app.update(); // Finish the deferred default-query-domain baseline.
    let before = counters(&app);
    let semantic = app.world().resource::<FarStock>().planning_work;
    app.update();
    let after = counters(&app);
    // The outer input gate now skips even the settled refresh probe.
    for (name, expected) in [
        ("far_stock/near_poll_frames", 0),
        ("far_stock/near_reference_candidate_rows_upper", 0),
        ("far_stock/near_reference_changed_yields", 0),
        ("far_stock/near_root_candidate_rows_upper", 0),
        ("far_stock/near_root_changed_yields", 0),
        ("far_stock/near_removal_stream_polls", 0),
        ("far_stock/near_removed_events", 0),
        ("far_stock/near_index_inserts", 0),
    ] {
        assert_eq!(
            count(&after, name) - count(&before, name),
            expected,
            "{name}"
        );
    }
    assert_eq!(app.world().resource::<FarStock>().planning_work, semantic);
    // Equal-value journal notifications must not masquerade as index inserts.
    let entity = app
        .world_mut()
        .query::<(Entity, &FormId)>()
        .iter(app.world())
        .find(|(_, form)| form.0 == u32::MAX)
        .unwrap()
        .0;
    app.world_mut().entity_mut(entity).insert(FormId(u32::MAX));
    app.update();
    let equal = counters(&app);
    assert_eq!(
        count(&equal, "far_stock/near_reference_changed_yields")
            - count(&after, "far_stock/near_reference_changed_yields"),
        1
    );
    assert_eq!(
        count(&equal, "far_stock/near_index_replacements")
            - count(&after, "far_stock/near_index_replacements"),
        0
    );
    app.world_mut().entity_mut(entity).insert(CellRef(1));
    app.update();
    let replaced = counters(&app);
    assert_eq!(
        count(&replaced, "far_stock/near_index_replacements")
            - count(&equal, "far_stock/near_index_replacements"),
        1
    );
}

#[test]
fn cell_load_and_despawn_report_index_insertions_and_coalesced_removals() {
    let fixture = Fixture::new();
    let key = fixture.selected().0;
    let mut app = complete_plan_app(&fixture, key);
    let parent = root(app.world_mut(), key, false);
    app.update();
    app.update();
    let before = counters(&app);
    let entities: Vec<_> = (0..16)
        .map(|id| {
            app.world_mut()
                .spawn((FormId(u32::MAX - id), CellRef(0), ChildOf(parent)))
                .id()
        })
        .collect();
    app.update();
    let loaded = counters(&app);
    for name in [
        "far_stock/near_index_inserts",
        "far_stock/near_reference_changed_yields",
    ] {
        assert_eq!(count(&loaded, name) - count(&before, name), 16, "{name}");
    }
    for entity in entities {
        app.world_mut().despawn(entity);
    }
    app.update();
    let removed = counters(&app);
    assert_eq!(
        count(&removed, "far_stock/near_removed_events")
            - count(&loaded, "far_stock/near_removed_events"),
        16
    );
    assert_eq!(
        count(&removed, "far_stock/near_index_removals")
            - count(&loaded, "far_stock/near_index_removals"),
        16
    );
    app.world_mut()
        .entity_mut(parent)
        .remove::<StreamedCellRoot>();
    app.update();
    let root_changed = counters(&app);
    assert_eq!(
        count(&root_changed, "far_stock/near_root_changed_yields")
            - count(&removed, "far_stock/near_root_changed_yields"),
        1
    );
    app.world_mut().despawn(parent);
    app.update();
    let root_removed = counters(&app);
    // Removing the remaining root identity components coalesces to one exact root candidate.
    assert_eq!(
        count(&root_removed, "far_stock/near_removed_events")
            - count(&root_changed, "far_stock/near_removed_events"),
        1
    );
}

#[test]
fn removing_a_still_desired_owner_readopts_unchanged_live_near_source() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    let parent = root(app.world_mut(), key, false);
    let entity = app
        .world_mut()
        .spawn((FormId(id.1), CellRef(row.cell_id), ChildOf(parent)))
        .id();
    app.update();
    app.update();
    assert_eq!(
        app.world().resource::<FarStock>().owners[&id].entity,
        entity
    );
    assert!(
        app.world_mut()
            .resource_mut::<FarStock>()
            .remove_owner(&id)
            .is_some()
    );
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(far.desired.contains_key(&id));
    assert_eq!(
        far.owners.get(&id).map(|owner| owner.entity),
        Some(entity),
        "owner-only dirtiness must rediscover an unchanged valid source"
    );
    assert!(
        far.queued.iter().all(|(_, ids)| !ids.contains(&id.1)),
        "readoption must prevent a second source request"
    );
    assert_bookkeeping(far);
}
