//! Enabled runtime refusals keep the ordinary source path at every distance.

use super::*;
use crate::world::reference_state::{self, ReferenceEnableState, SetReferenceEnabled};

fn fixture() -> Fixture {
    Fixture::from_directory(
        crate::cluster::reader::producer_fixture_tests::published_for_runtime_draw_states(),
    )
}

fn app(fixture: &Fixture) -> App {
    let mut app = complete_plan_app(fixture, exterior(0x70, IVec2::new(200, 0)));
    reference_state::install(&mut app);
    app.world_mut()
        .resource_mut::<ReferenceEnableState>()
        .bind(fixture.package.enable_catalog().clone());
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    app
}

fn desired(app: &App, reference: u32) -> bool {
    app.world()
        .resource::<FarStock>()
        .desired
        .contains_key(&(0x70, reference))
}

fn set(app: &mut App, reference: u32, enabled: bool) {
    app.world_mut()
        .trigger(SetReferenceEnabled { reference, enabled });
    app.update();
}

#[test]
fn crossings_reuse_unbounded_desired_candidates() {
    let fixture = fixture();
    let mut app = app(&fixture);
    let candidates = |app: &App| {
        let work = app.world().resource::<FarStock>().planning_work;
        work.bounded_entries + work.unbounded_entries
    };
    app.update();
    let desired = app.world().resource::<FarStock>().desired.clone();
    assert!(
        !desired.is_empty(),
        "the fixture must demand distant sources"
    );
    let initial_visits = candidates(&app);
    assert!(
        initial_visits > 0,
        "initial qualification must visit candidates"
    );
    let initial_center = app.world().resource::<RenderOrigin>().0;
    for offset in 0..=3 {
        let center = initial_center + IVec2::X * offset;
        assert!(
            fixture
                .package
                .placed_cells_in_window(0x70, center, Some(0))
                .next()
                .is_none(),
            "the crossing must not change the bounded source overlay"
        );
        if offset == 0 {
            continue;
        }
        app.world_mut().resource_mut::<RenderOrigin>().0 = center;
        app.update();
        assert_eq!(
            app.world().resource::<FarStock>().desired,
            desired,
            "an empty neighboring cell must retain the exact desired identities and cells"
        );
    }
    assert_eq!(
        candidates(&app) - initial_visits,
        0,
        "empty-cell crossings must reuse the unbounded desired candidates"
    );
}

#[test]
fn pending_response_survives_demand_only_crossing_without_duplicate_admission() {
    let fixture = fixture();
    let mut app = app(&fixture);
    app.update();
    let desired = app.world().resource::<FarStock>().desired.clone();
    let (responses, receiver) = crossbeam_channel::bounded(1);
    let (key, ids, generation) = {
        let mut far = app.world_mut().resource_mut::<FarStock>();
        let (key, ids) = far.pop_queued().expect("distant sources must be queued");
        far.pending.insert(
            key,
            Pending {
                ids: ids.clone(),
                receiver,
                ready: None,
            },
        );
        (key, ids, far.generation)
    };
    let rows: Vec<_> = fixture
        .rows
        .iter()
        .filter(|(cell, row)| *cell == key && ids.contains(&row.form_id))
        .map(|(_, row)| row.clone())
        .collect();
    assert_eq!(rows.len(), ids.len());
    app.world_mut().resource_mut::<RenderOrigin>().0.x += 1;
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.desired, desired,
        "the exact target set must be unchanged"
    );
    responses
        .try_send(ReferenceResponse {
            generation,
            key,
            result: Ok(rows),
            query_micros: 0,
            queue_wait_micros: 0,
            total_request_micros: 0,
        })
        .expect("an identical target set must retain the original in-flight receiver");
    assert_eq!(far.pending.len(), 1);
    assert_eq!(far.pending[&key].ids, ids);
    assert!(
        !far.queued_keys.contains(&key) && far.queued.iter().all(|(cell, _)| *cell != key),
        "the surviving request must not be admitted to the sparse queue a second time"
    );
    let response = far.pending[&key].receiver.try_recv().unwrap();
    assert_eq!(
        response.generation, far.generation,
        "the retained response must remain valid for publication"
    );
    assert_eq!(response.key, key);
    assert_eq!(
        response
            .result
            .as_ref()
            .unwrap()
            .iter()
            .map(|row| row.form_id)
            .collect::<BTreeSet<_>>(),
        ids
    );
    app.world_mut()
        .resource_mut::<FarStock>()
        .pending
        .get_mut(&key)
        .unwrap()
        .ready = Some(response);
    app.world_mut().resource_mut::<RenderOrigin>().0.x += 1;
    app.update();
    let far = app.world().resource::<FarStock>();
    let ready = far.pending[&key]
        .ready
        .as_ref()
        .expect("a held ready response must survive another demand-only crossing");
    assert_eq!(ready.generation, generation);
    assert_eq!(far.generation, generation);
    assert_eq!(ready.key, key);
    assert!(!far.queued_keys.contains(&key));
    assert_no_exit(app.world());
}

#[test]
fn desired_union_preserves_overlaps_and_tracks_exact_cell_changes() {
    let mut far = FarStock::default();
    let id = (17, 42);
    let key = exterior(id.0, IVec2::ZERO);
    let contribution = HashMap::from([(id, key)]);
    far.unbounded_desired = contribution.clone();
    far.bounded_desired = contribution.clone();
    assert!(far.project_union(HashSet::from([id])));
    far.bounded_desired.clear();
    assert!(!far.project_union(HashSet::from([id])));
    assert_eq!(far.desired, contribution);

    // Near enrollment must remain tracked when the cached unbounded projection is invalidated.
    far.enroll_bounded(id, key);
    far.unbounded_desired.clear();
    assert!(!far.project_union(HashSet::from([id])));
    assert_eq!(far.desired, contribution);
    let moved = exterior(id.0, IVec2::X);
    far.bounded_desired.insert(id, moved);
    assert!(far.project_union(HashSet::from([id])));
    assert_eq!(far.desired.get(&id), Some(&moved));
    far.bounded_desired.clear();
    assert!(far.project_union(HashSet::from([id])));
    assert!(far.desired.is_empty());
}

#[test]
fn demand_only_changes_do_no_inventory_walks_after_first_qualification() {
    let fixture = fixture();
    let mut app = app(&fixture);
    app.update();
    let generation = app.world().resource::<FarStock>().generation;
    let center = app.world().resource::<FarStock>().demand.unwrap().center;
    for _ in 0..3 {
        app.world_mut().resource_mut::<RenderOrigin>().0.x += 1;
        app.update();
    }
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.generation, generation);
    assert_eq!(far.demand.unwrap().center, center + IVec2::X * 3);
    set(&mut app, 0x110, false);
    app.update();
    assert_eq!(
        app.world()
            .resource::<ReferenceEnableState>()
            .enabled(0x110),
        Some(false)
    );
}

#[test]
fn bounded_overlap_keeps_unbounded_demand_until_its_enable_state_changes() {
    let fixture = fixture();
    let mut app = app(&fixture);
    let reference = fixture.package.placed_reference(0x111).unwrap();
    assert!(full_stock_reach(reference));
    let key = exterior(
        0x70,
        IVec2::new(reference.snapshot.cell.0, reference.snapshot.cell.1),
    );
    let id = (0x70, 0x111);
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(key);
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.unbounded_desired.get(&id), Some(&key));
    assert_eq!(far.bounded_desired.get(&id), Some(&key));

    app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::new(200, 0);
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(!far.bounded_desired.contains_key(&id));
    assert_eq!(far.desired.get(&id), Some(&key));
    let generation = far.generation;
    set(&mut app, 0x110, false);
    let far = app.world().resource::<FarStock>();
    assert!(!far.unbounded_desired.contains_key(&id));
    assert!(!far.desired.contains_key(&id));
    assert_ne!(far.generation, generation);
    set(&mut app, 0x110, true);
    assert_eq!(
        app.world().resource::<FarStock>().desired.get(&id),
        Some(&key)
    );
}

#[test]
fn pre_bind_queued_disable_suppresses_owned_instances_at_first_qualification() {
    let fixture = fixture();
    assert!(fixture.package.reference(0x100).is_some());
    let mut app = complete_plan_app(&fixture, exterior(0x70, IVec2::new(200, 0)));
    reference_state::install(&mut app);
    crate::cluster::runtime_state::install(&mut app);
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .cluster_own_statics = true;
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    app.world_mut().trigger(SetReferenceEnabled {
        reference: 0x100,
        enabled: false,
    });
    app.update();
    assert_eq!(
        app.world()
            .resource::<ReferenceEnableState>()
            .enabled(0x100),
        Some(false)
    );
    assert!(
        app.world()
            .resource::<crate::cluster::runtime_state::RuntimeReferenceState>()
            .contains(0x100)
    );
    set(&mut app, 0x100, true);
    assert!(
        !app.world()
            .resource::<crate::cluster::runtime_state::RuntimeReferenceState>()
            .contains(0x100)
    );
}

#[test]
fn equal_enable_inputs_reproject_suppression_when_certificate_membership_changes() {
    let mapped = Fixture::new();
    let unmapped = Fixture::from_directory(published_for_legacy_threshold_inventory());
    assert!(mapped.package.reference(0x100).is_some());
    assert!(unmapped.package.reference(0x100).is_none());
    let mut app = app(&mapped);
    crate::cluster::runtime_state::install(&mut app);
    app.update();
    set(&mut app, 0x100, false);
    assert!(
        app.world()
            .resource::<crate::cluster::runtime_state::RuntimeReferenceState>()
            .contains(0x100)
    );
    let enable_revision = app.world().resource::<ReferenceEnableState>().revision();
    for (fixture, suppressed) in [(&unmapped, false), (&mapped, true)] {
        app.insert_resource(ValidatedPackage(fixture.package.clone()));
        app.world_mut()
            .resource_mut::<SourceFallbackDemand>()
            .update(Some(&fixture.package), BTreeSet::new());
        app.update();
        assert_eq!(
            app.world().resource::<ReferenceEnableState>().revision(),
            enable_revision
        );
        assert_eq!(
            app.world()
                .resource::<ReferenceEnableState>()
                .enabled(0x100),
            Some(false)
        );
        assert_eq!(
            app.world()
                .resource::<crate::cluster::runtime_state::RuntimeReferenceState>()
                .contains(0x100),
            suppressed
        );
    }
}

#[test]
fn distant_enable_parent_and_opposite_follow_runtime_state_without_settled_replanning() {
    let fixture = fixture();
    let mut app = app(&fixture);
    app.update();
    assert!(
        desired(&app, 0x111),
        "enabled parent must provide a distant source route"
    );
    assert!(!desired(&app, 0x112));
    set(&mut app, 0x110, false);
    assert!(!desired(&app, 0x111));
    assert!(desired(&app, 0x112));
    set(&mut app, 0x110, true);
    assert!(desired(&app, 0x111));
    assert!(!desired(&app, 0x112));
    let generation = app.world().resource::<FarStock>().generation;
    let revision = app.world().resource::<ReferenceEnableState>().revision();
    for _ in 0..8 {
        app.update();
    }
    assert_eq!(app.world().resource::<FarStock>().generation, generation);
    assert_eq!(
        app.world().resource::<ReferenceEnableState>().revision(),
        revision
    );
}

#[test]
fn initially_disabled_distant_static_waits_for_enable_and_scripted_static_uses_state() {
    let fixture = fixture();
    let mut app = app(&fixture);
    app.update();
    assert!(!desired(&app, 0x109));
    assert!(
        desired(&app, 0x113),
        "a script attachment does not mean disabled"
    );
    set(&mut app, 0x109, true);
    assert!(desired(&app, 0x109));
    set(&mut app, 0x113, false);
    assert!(!desired(&app, 0x113));
    set(&mut app, 0x113, true);
    assert!(desired(&app, 0x113));
}

#[test]
fn distant_runtime_sources_publish_current_state_before_scene_admission() {
    let fixture = fixture();
    for (reference, controller) in [(0x109, 0x109), (0x111, 0x110), (0x113, 0x113)] {
        let (key, row) = fixture
            .rows
            .iter()
            .find(|(_, row)| row.form_id == reference)
            .unwrap();
        let mut app = layer_app(&fixture, *key, row);
        reference_state::install(&mut app);
        app.world_mut()
            .resource_mut::<ReferenceEnableState>()
            .bind(fixture.package.enable_catalog().clone());
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        app.world_mut()
            .resource_mut::<FarStock>()
            .demand
            .as_mut()
            .unwrap()
            .center += IVec2::new(200, 0);
        if !row.initially_enabled {
            app.world_mut().trigger(SetReferenceEnabled {
                reference,
                enabled: true,
            });
        }
        app.update();
        let identity = (cell_world(*key).unwrap(), reference);
        let root = app
            .world()
            .resource::<FarStock>()
            .owners
            .get(&identity)
            .expect("enabled runtime-refused static must publish an ordinary source")
            .entity;
        assert!(
            app.world()
                .get::<crate::streaming::requests::SceneRequest>(root)
                .is_some()
        );
        // The complete reference was refused: its whole ordinary body is needed.
        assert!(
            app.world()
                .get::<crate::streaming::partial_stock::PartialStock>(root)
                .is_none()
        );
        app.world_mut().trigger(SetReferenceEnabled {
            reference: controller,
            enabled: false,
        });
        assert_eq!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
        app.world_mut().trigger(SetReferenceEnabled {
            reference: controller,
            enabled: true,
        });
        assert_ne!(
            app.world().get::<Visibility>(root),
            Some(&Visibility::Hidden)
        );
    }
}
