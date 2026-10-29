// Included inside cluster::render::handoff::readiness_bridge::tests.
mod runtime_policy {
    use super::*;
    use crate::{
        cluster::{
            ownership::{OwnedPrimitiveSource, OwnershipFacts},
            package::{LazyBatches, ValidatedPackage},
            runtime_state::{ReferenceStateChanged, RestingReferencePose, RuntimeReferenceState},
        },
        pacing::LoadingCover,
        streaming::{ActiveSpace, RenderOrigin, StreamingMetrics, StreamingWorld},
        world::{
            components::{FormId, StreamingCamera},
            database::CellKey,
            reference_state::{self, ReferenceEnableState, SetReferenceEnabled},
        },
    };
    use bevy::{
        ecs::{schedule::SingleThreadedExecutor, system::RunSystemOnce},
        world_serialization::WorldAssetRoot,
    };

    struct Fixture {
        app: App,
        render: World,
        package: Arc<Certificate>,
        inventory: Arc<Inventory>,
        reference: u32,
        world: u32,
        root: Entity,
        source: Entity,
        layout: u64,
        frame: u64,
        _directory: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
            let package = Arc::new(
                crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
            );
            let (_, identity) = package.records().next().unwrap();
            let reference = identity.reference;
            let world = identity.worldspace;
            let lazy = LazyBatches::unrequested_for_test(1, Vec3::ZERO);
            let layout = lazy.layout_scope;
            let instances: Vec<_> = package
                .records()
                .map(|(record, identity)| ClusterInstance {
                    identity: ClusterInstanceIdentity::Object(*record),
                    mesh: 0,
                    batch: 0,
                    world_from_local: Mat4::IDENTITY,
                    source_worldspace: identity.worldspace,
                })
                .collect();
            let inventory =
                Arc::new(Inventory::build(package.clone(), layout, &instances, 1).unwrap());
            let mut facts = OwnershipFacts::default();
            facts.publish(
                &package,
                package
                    .records()
                    .map(|(record, _)| (*record, false))
                    .collect(),
            );
            assert!(
                facts.reference_has_owned(reference),
                "fixture must exercise an actually owned certified reference"
            );
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(crate::config::EngineConfig {
                    clusters: Some("runtime-policy-fixture".into()),
                    cluster_own_statics: true,
                    worldspace_id: world,
                    stream_radius: 0,
                    ..default()
                })
                .insert_resource(ValidatedPackage(package.clone()))
                .insert_resource(lazy)
                .insert_resource(facts)
                .insert_resource(ClusterScene {
                    ready_revision: default(),
                    residency_revision: default(),
                    certificate: Some(package.clone()),
                    inventory: Some(inventory.clone()),
                    terrain_blocks: vec![],
                    meshes: vec![],
                    instances,
                    origin_offset: Vec3::ZERO,
                    blocks: vec![],
                })
                .init_resource::<ActiveSpace>()
                .insert_resource(RenderOrigin(IVec2::ZERO))
                .init_resource::<StreamingWorld>()
                .init_resource::<StreamingMetrics>();
            crate::cluster::runtime_state::install(&mut app);
            crate::cluster::loading::install(&mut app);
            reference_state::install(&mut app);
            app.world_mut()
                .resource_mut::<ReferenceEnableState>()
                .bind(package.enable_catalog().clone());
            app.edit_schedule(PostUpdate, |schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            });
            app.world_mut()
                .spawn((StreamingCamera, Transform::default()));
            let cell = app.world_mut().spawn_empty().id();
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .set_resident_for_test(
                    CellKey::Exterior {
                        worldspace_id: world,
                        grid_x: 0,
                        grid_y: 0,
                    },
                    cell,
                );
            let root = app
                .world_mut()
                .spawn((
                    FormId(reference),
                    RestingReferencePose(Transform::default()),
                    Transform::default(),
                    Visibility::Inherited,
                ))
                .id();
            let source = app
                .world_mut()
                .spawn((
                    ChildOf(root),
                    OwnedPrimitiveSource {
                        mesh: Mesh3d(Handle::default()),
                        material: None,
                    },
                ))
                .id();
            app.update();
            assert!(
                !app.world()
                    .resource::<RuntimeReferenceState>()
                    .contains(reference)
            );
            let mut render = World::new();
            render.init_resource::<Producer>();
            Self {
                app,
                render,
                package,
                inventory,
                reference,
                world,
                root,
                source,
                layout,
                frame: 0,
                _directory: directory,
            }
        }

        fn enabled(&mut self, enabled: bool) {
            self.app.world_mut().trigger(SetReferenceEnabled {
                reference: self.reference,
                enabled,
            });
            self.app.update();
        }

        // These are the actual native catalog, retained aggregate and publication owners.
        // Prepared batch/consumer facts are protocol inputs, not GPU execution evidence.
        // Keep this input seam aligned with observe(): no permanent/disabled test evaluator.
        fn observe_and_publish(&mut self) -> HashMap<u32, ClusterWorldReadiness> {
            self.observe_and_publish_with_inputs(
                (true, true, false, false),
                None,
                BTreeSet::new(),
                BTreeSet::new(),
            )
        }

        fn observe_and_publish_with_inputs(
            &mut self,
            batch_facts: (bool, bool, bool, bool),
            refusal_revision: Option<u64>,
            refused_records: BTreeSet<crate::cluster::reader::RecordKey>,
            changed_batches: BTreeSet<usize>,
        ) -> HashMap<u32, ClusterWorldReadiness> {
            let state = self.app.world().resource::<RuntimeReferenceState>().clone();
            let generation = self
                .app
                .world()
                .resource::<ClusterLoadingGate>()
                .generation();
            let scene_tick = self
                .app
                .world()
                .get_resource_ref::<ClusterScene>()
                .unwrap()
                .last_changed();
            let mut key = catalog_key();
            key.catalog.scene_changed = scene_tick;
            key.catalog.layout_scope = self.layout;
            key.catalog.refusal_revision = refusal_revision;
            key.catalog.runtime_state_revision = Some(state.revision);
            let consumers: Vec<_> = self
                .package
                .records()
                .map(|(_, record)| record.worldspace)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .enumerate()
                .map(|(index, world)| Consumer {
                    world,
                    ..consumer(index as u32 + 1, false, Some(Arc::new(vec![true])))
                })
                .collect();
            let inventory = self.inventory.clone();
            let package = self.package.clone();
            let worlds = self
                .render
                .resource_mut::<Producer>()
                .observe_incremental(
                    key,
                    [
                        generation,
                        1,
                        1,
                        self.layout,
                        0,
                        0,
                        1,
                        self.layout,
                        state.revision,
                    ],
                    consumers,
                    |producer| {
                        producer.update_inventory(
                            &inventory,
                            &package,
                            refusal_revision,
                            &refused_records,
                            Some(state.revision),
                            || state.readiness_references().collect(),
                        )
                    },
                    changed_batches,
                    true,
                    |_, _| batch_facts,
                )
                .expect("forced protocol publication must derive a current aggregate");
            self.frame += 1;
            // No test supplies failed_batches/invalid_owned_state: worlds came entirely from
            // the real inventory/aggregate path above, then passes the real coherence gate.
            self.render.resource_mut::<Producer>().pending = Some(Snapshot {
                package: self.package.clone(),
                inventory: self.inventory.clone(),
                layout: self.layout,
                generation,
                frame: self.frame,
                worlds: worlds.clone(),
            });
            let mut main = MainWorld::default();
            std::mem::swap(self.app.world_mut(), &mut *main);
            self.render.insert_resource(main);
            self.render.run_system_once(publish).unwrap();
            let mut main = self.render.remove_resource::<MainWorld>().unwrap();
            std::mem::swap(self.app.world_mut(), &mut *main);
            worlds
        }

        fn assert_no_stock_body(&mut self) {
            assert!(
                self.app
                    .world()
                    .get::<OwnedPrimitiveSource>(self.source)
                    .is_some()
            );
            assert!(self.app.world().get::<Mesh3d>(self.source).is_none());
            assert!(self.app.world().get::<WorldAssetRoot>(self.root).is_none());
            let world = self.app.world_mut();
            assert_eq!(world.query::<&Mesh3d>().iter(world).count(), 0);
            assert_eq!(world.query::<&WorldAssetRoot>().iter(world).count(), 0);
            assert!(
                world
                    .resource::<OwnershipFacts>()
                    .reference_has_owned(self.reference)
            );
            assert!(
                world
                    .resource::<OwnershipFacts>()
                    .permanent_refused_records()
                    .is_empty()
            );
        }

        fn begin_later_teleport(&mut self) {
            self.app
                .world_mut()
                .run_system_once(|mut cover: ResMut<LoadingCover>, mut commands: Commands| {
                    cover.begin_teleport(&mut commands);
                })
                .unwrap();
            self.app.update(); // The actual owner invalidates old generation/pose proof.
            assert!(
                self.app
                    .world()
                    .resource::<LoadingCover>()
                    .teleport_pending()
            );
        }
    }

    #[test]
    fn reversible_disable_does_not_become_a_failed_owned_batch_at_real_publication() {
        let mut fixture = Fixture::new();
        let ready = fixture.observe_and_publish();
        assert_eq!(ready[&fixture.world].failed_batches, 0);
        assert!(
            fixture
                .app
                .world()
                .resource::<ClusterLoadingGate>()
                .world_ready(fixture.world)
        );
        fixture.enabled(false);
        assert!(
            fixture
                .app
                .world()
                .resource::<RuntimeReferenceState>()
                .contains(fixture.reference)
        );
        assert_eq!(
            fixture.app.world().get::<Visibility>(fixture.root),
            Some(&Visibility::Hidden)
        );
        let hidden = fixture.observe_and_publish();
        assert_eq!(
            hidden[&fixture.world].failed_batches, 0,
            "legitimate reversible enable suppression is not permanent invalid-owned-state"
        );
        assert!(
            fixture
                .app
                .world()
                .get_resource::<crate::streaming::failure::LoadingRefusal>()
                .is_none()
        );
        assert!(
            fixture
                .app
                .world()
                .resource::<ClusterLoadingGate>()
                .world_ready(fixture.world)
        );
        fixture.assert_no_stock_body();
    }

    #[test]
    fn disabled_owned_reference_allows_a_later_real_teleport_and_reenable_restores_visibility() {
        let mut fixture = Fixture::new();
        fixture.observe_and_publish(); // Establish that initial startup proof was accepted.
        fixture.enabled(false);
        fixture.begin_later_teleport();
        let hidden = fixture.observe_and_publish();
        assert_eq!(
            hidden[&fixture.world].failed_batches, 0,
            "hidden legitimate instances must not poison a later destination generation"
        );
        fixture.app.update(); // Actual finish_teleport_load, not a replacement release predicate.
        assert!(
            !fixture
                .app
                .world()
                .resource::<LoadingCover>()
                .teleport_pending()
        );
        assert_eq!(
            crate::pacing::teleport_cover_count(fixture.app.world_mut()),
            0
        );
        assert_eq!(
            fixture.app.world().get::<Visibility>(fixture.root),
            Some(&Visibility::Hidden)
        );
        fixture.enabled(true);
        assert!(
            !fixture
                .app
                .world()
                .resource::<RuntimeReferenceState>()
                .contains(fixture.reference)
        );
        assert_eq!(
            fixture.app.world().get::<Visibility>(fixture.root),
            Some(&Visibility::Inherited)
        );
        let restored = fixture.observe_and_publish();
        assert_eq!(restored[&fixture.world].failed_batches, 0);
        assert!(
            fixture
                .app
                .world()
                .resource::<ClusterLoadingGate>()
                .world_ready(fixture.world)
        );
        assert!(
            fixture
                .app
                .world()
                .get_resource::<crate::streaming::failure::LoadingRefusal>()
                .is_none()
        );
        fixture.assert_no_stock_body();
    }

    fn assert_terminal_pose_reason(fixture: &mut Fixture) {
        let refusal = &fixture
            .app
            .world()
            .get_resource::<crate::streaming::failure::LoadingRefusal>()
            .expect(
                "permanent owned pose revocation must refuse immediately at its real runtime owner",
            )
            .0;
        assert!(
            refusal.contains(&fixture.reference.to_string())
                || refusal
                    .to_ascii_lowercase()
                    .contains(&format!("{:x}", fixture.reference)),
            "reference absent from terminal diagnostic: {refusal}"
        );
        assert!(
            refusal.contains("pose") || refusal.contains("transform") || refusal.contains("moved"),
            "permanent revocation cause absent from terminal diagnostic: {refusal}"
        );
        fixture.assert_no_stock_body();
    }

    #[test]
    fn permanent_owned_pose_revocation_after_startup_refuses_once_with_reference_and_cause() {
        let mut fixture = Fixture::new();
        fixture.observe_and_publish();
        assert!(
            fixture
                .app
                .world()
                .resource::<ClusterLoadingGate>()
                .world_ready(fixture.world)
        );
        let mut exits = fixture
            .app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        fixture
            .app
            .world_mut()
            .get_mut::<Transform>(fixture.root)
            .unwrap()
            .translation
            .x += 1.0;
        fixture.app.update();
        assert_terminal_pose_reason(&mut fixture);
        let reason = fixture
            .app
            .world()
            .resource::<crate::streaming::failure::LoadingRefusal>()
            .0
            .clone();
        assert_eq!(
            exits
                .read(fixture.app.world().resource::<Messages<AppExit>>())
                .cloned()
                .collect::<Vec<_>>(),
            [AppExit::error()]
        );
        for _ in 0..3 {
            fixture.app.update();
            fixture.observe_and_publish();
            assert_eq!(
                fixture
                    .app
                    .world()
                    .resource::<crate::streaming::failure::LoadingRefusal>()
                    .0,
                reason
            );
            assert_eq!(
                exits
                    .read(fixture.app.world().resource::<Messages<AppExit>>())
                    .count(),
                0
            );
            fixture.assert_no_stock_body();
        }
    }

    #[test]
    fn disabled_then_permanently_moved_reference_notifies_policy_without_becoming_reversible() {
        let mut fixture = Fixture::new();
        fixture.observe_and_publish();
        fixture.enabled(false);
        let disabled_revision = fixture
            .app
            .world()
            .resource::<RuntimeReferenceState>()
            .revision;
        fixture
            .app
            .world_mut()
            .get_mut::<Transform>(fixture.root)
            .unwrap()
            .translation
            .y += 1.0;
        fixture.app.update();
        assert!(
            fixture
                .app
                .world()
                .resource::<RuntimeReferenceState>()
                .revision
                > disabled_revision,
            "a new permanent revocation must notify readiness even if union membership was already disabled"
        );
        assert_terminal_pose_reason(&mut fixture);
        fixture.enabled(true);
        assert!(
            fixture
                .app
                .world()
                .resource::<RuntimeReferenceState>()
                .contains(fixture.reference),
            "reenable must never erase an independent permanent pose revocation"
        );
        fixture.assert_no_stock_body();
    }

    #[test]
    fn explicit_logical_revocation_after_startup_is_terminal_without_inventing_a_gameplay_sender() {
        let mut fixture = Fixture::new();
        fixture.observe_and_publish();
        let mut exits = fixture
            .app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        fixture.app.world_mut().trigger(ReferenceStateChanged {
            reference: fixture.reference,
        });
        fixture.app.world_mut().flush();
        let refusal = &fixture
            .app
            .world()
            .get_resource::<crate::streaming::failure::LoadingRefusal>()
            .expect("the existing public logical-revocation observer must own terminal policy")
            .0;
        assert!(
            refusal.contains(&fixture.reference.to_string())
                || refusal
                    .to_ascii_lowercase()
                    .contains(&format!("{:x}", fixture.reference))
        );
        assert!(
            refusal.contains("logical") || refusal.contains("state") || refusal.contains("changed")
        );
        assert_eq!(
            exits
                .read(fixture.app.world().resource::<Messages<AppExit>>())
                .cloned()
                .collect::<Vec<_>>(),
            [AppExit::error()]
        );
        fixture.app.world_mut().trigger(ReferenceStateChanged {
            reference: fixture.reference,
        });
        fixture.app.world_mut().flush();
        assert_eq!(
            exits
                .read(fixture.app.world().resource::<Messages<AppExit>>())
                .count(),
            0
        );
        fixture.assert_no_stock_body();
    }
    #[test]
    fn logical_revocation_waits_for_actual_ownership_qualification_then_refuses_once() {
        let mut fixture = Fixture::new();
        fixture.app.insert_resource(OwnershipFacts::default());
        let mut exits = fixture
            .app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        fixture.app.world_mut().trigger(ReferenceStateChanged {
            reference: fixture.reference,
        });
        fixture.app.world_mut().flush();
        fixture.app.update();
        assert!(
            fixture
                .app
                .world()
                .resource::<RuntimeReferenceState>()
                .contains(fixture.reference)
        );
        assert!(
            fixture
                .app
                .world()
                .get_resource::<crate::streaming::failure::LoadingRefusal>()
                .is_none(),
            "unqualified state is not proof of an owned cluster reference"
        );
        assert_eq!(
            exits
                .read(fixture.app.world().resource::<Messages<AppExit>>())
                .count(),
            0
        );
        fixture
            .app
            .world_mut()
            .resource_mut::<OwnershipFacts>()
            .publish(
                &fixture.package,
                fixture
                    .package
                    .records()
                    .map(|(record, _)| (*record, false))
                    .collect(),
            );
        fixture.app.update();
        let reason = fixture
            .app
            .world()
            .get_resource::<crate::streaming::failure::LoadingRefusal>()
            .expect(
                "late real ownership qualification must commit the retained permanent revocation",
            )
            .0
            .clone();
        assert!(
            reason.contains(&fixture.reference.to_string())
                || reason
                    .to_ascii_lowercase()
                    .contains(&format!("{:x}", fixture.reference))
        );
        assert!(
            reason.contains("logical") || reason.contains("state") || reason.contains("changed")
        );
        assert_eq!(
            exits
                .read(fixture.app.world().resource::<Messages<AppExit>>())
                .cloned()
                .collect::<Vec<_>>(),
            [AppExit::error()]
        );
        for _ in 0..3 {
            fixture.app.update();
            assert_eq!(
                fixture
                    .app
                    .world()
                    .resource::<crate::streaming::failure::LoadingRefusal>()
                    .0,
                reason
            );
            assert_eq!(
                exits
                    .read(fixture.app.world().resource::<Messages<AppExit>>())
                    .count(),
                0
            );
        }
        fixture.assert_no_stock_body();
    }
    #[test]
    fn unqualified_permanent_revocation_cannot_publish_generic_failure_before_owned_or_refused_decision()
     {
        for material_refused in [false, true] {
            let mut fixture = Fixture::new();
            fixture.app.insert_resource(OwnershipFacts::default());
            fixture
                .app
                .world_mut()
                .resource_mut::<OwnershipFacts>()
                .publish_updates(&fixture.package, true, std::iter::empty());
            assert!(
                fixture
                    .app
                    .world()
                    .resource::<OwnershipFacts>()
                    .package_matches(&fixture.package)
            );
            assert!(
                !fixture
                    .app
                    .world()
                    .resource::<OwnershipFacts>()
                    .can_spawn(fixture.reference)
            );
            let mut exits = fixture
                .app
                .world()
                .resource::<Messages<AppExit>>()
                .get_cursor_current();
            fixture.app.world_mut().trigger(ReferenceStateChanged {
                reference: fixture.reference,
            });
            fixture.app.world_mut().flush();
            fixture.app.update();
            assert!(
                fixture
                    .app
                    .world()
                    .get_resource::<crate::streaming::failure::LoadingRefusal>()
                    .is_none()
            );
            let refusal_revision = fixture
                .app
                .world()
                .resource::<OwnershipFacts>()
                .refusal_revision();
            // These are the actual pending template facts which source_fallback_bridge refuses
            // to turn into terminal ownership. No failed-batch or invalid-owned flag is injected.
            for _ in 0..2 {
                let pending = fixture.observe_and_publish_with_inputs(
                    (true, false, false, false),
                    Some(refusal_revision),
                    BTreeSet::new(),
                    BTreeSet::new(),
                );
                assert!(pending[&fixture.world].pending_templates > 0);
                assert_eq!(
                    pending[&fixture.world].failed_batches, 0,
                    "unqualified permanent state is not an accepted owned renderer batch failure"
                );
                assert!(
                    fixture
                        .app
                        .world()
                        .get_resource::<crate::streaming::failure::LoadingRefusal>()
                        .is_none(),
                    "generic renderer publication must not beat the actual ownership/cause owner"
                );
                assert_eq!(
                    exits
                        .read(fixture.app.world().resource::<Messages<AppExit>>())
                        .count(),
                    0
                );
                fixture.app.update();
            }
            fixture
                .app
                .world_mut()
                .resource_mut::<OwnershipFacts>()
                .publish_updates(
                    &fixture.package,
                    false,
                    fixture
                        .package
                        .records()
                        .map(|(record, _)| (*record, Some(material_refused))),
                );
            fixture.app.update();
            let facts = fixture.app.world().resource::<OwnershipFacts>();
            assert_eq!(
                facts.reference_has_owned(fixture.reference),
                !material_refused
            );
            let refusal_revision = facts.refusal_revision();
            let refused_records = facts.permanent_refused_records();
            let expected_reason = fixture
                .app
                .world()
                .get_resource::<crate::streaming::failure::LoadingRefusal>()
                .map(|refusal| refusal.0.clone());
            if material_refused {
                assert!(
                    expected_reason.is_none(),
                    "the initially refused reference never became cluster-owned"
                );
                assert_eq!(
                    exits
                        .read(fixture.app.world().resource::<Messages<AppExit>>())
                        .count(),
                    0
                );
            } else {
                let reason = expected_reason
                    .as_ref()
                    .expect("accepted late ownership must commit the retained permanent cause");
                assert!(
                    reason
                        .to_ascii_lowercase()
                        .contains(&format!("{:x}", fixture.reference))
                );
                assert!(
                    reason.contains("logical")
                        || reason.contains("state")
                        || reason.contains("changed")
                );
                assert_eq!(
                    exits
                        .read(fixture.app.world().resource::<Messages<AppExit>>())
                        .cloned()
                        .collect::<Vec<_>>(),
                    [AppExit::error()]
                );
            }
            let changed = (0..fixture.inventory.batch_count()).collect();
            let _settled = fixture.observe_and_publish_with_inputs(
                (true, true, false, false),
                Some(refusal_revision),
                refused_records,
                changed,
            );
            assert_eq!(
                fixture
                    .app
                    .world()
                    .get_resource::<crate::streaming::failure::LoadingRefusal>()
                    .map(|refusal| refusal.0.clone()),
                expected_reason
            );
            assert_eq!(
                exits
                    .read(fixture.app.world().resource::<Messages<AppExit>>())
                    .count(),
                0
            );
        }
    }
}
