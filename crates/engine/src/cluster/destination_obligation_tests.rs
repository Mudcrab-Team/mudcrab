// Included inside the actual destination owner module's cfg(test) tests.
mod destination_obligations {
    use super::*;
    use crate::{
        cluster::{
            package::ValidatedPackage,
            reader::{
                CertifiedPackage, RecordKey, producer_fixture_tests::published_for_readiness,
            },
            runtime_state::{ReferenceStateChanged, RuntimeReferenceState},
        },
        world::reference_state::{self, ReferenceEnableState, SetReferenceEnabled},
    };
    use std::collections::BTreeSet;

    struct Fixture {
        app: App,
        package: Arc<CertifiedPackage>,
        sources: Sources,
        world: u32,
        grid: IVec2,
        target: u32,
        sibling: u32,
        other_world: u32,
        other_grid: IVec2,
        _directory: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = published_for_readiness();
            let package = Arc::new(
                crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
            );
            let worlds: BTreeSet<_> = package
                .records()
                .map(|(_, record)| record.worldspace)
                .collect();
            let (world, grid, target, sibling) = worlds
                .iter()
                .find_map(|&world| {
                    package
                        .placed_cells_in_window(world, IVec2::ZERO, None)
                        .find_map(|(grid, _)| {
                            let references: BTreeSet<_> = package
                                .records_in_placed_cell(world, grid)
                                .map(|(_, record)| record.reference)
                                .collect();
                            let mut references = references.into_iter();
                            let target = references.next()?;
                            let sibling = references.next()?;
                            Some((world, grid, target, sibling))
                        })
                })
                .expect("certified fixture must have two accepted references in one placing cell");
            let other_world = *worlds
                .iter()
                .find(|&&candidate| candidate != world)
                .expect("certified fixture has a second placed world");
            let other_grid = package
                .placed_cells_in_window(other_world, IVec2::ZERO, None)
                .find_map(|(grid, _)| {
                    package
                        .records_in_placed_cell(other_world, grid)
                        .next()
                        .map(|_| grid)
                })
                .unwrap();
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(ValidatedPackage(package.clone()));
            crate::cluster::runtime_state::install(&mut app);
            reference_state::install(&mut app);
            app.world_mut()
                .resource_mut::<ReferenceEnableState>()
                .bind(package.enable_catalog().clone());
            app.update();
            Self {
                app,
                package,
                sources: Sources::default(),
                world,
                grid,
                target,
                sibling,
                other_world,
                other_grid,
                _directory: directory,
            }
        }

        fn enabled(&mut self, enabled: bool) {
            self.app.world_mut().trigger(SetReferenceEnabled {
                reference: self.target,
                enabled,
            });
            self.app.world_mut().flush();
            self.app.update();
            assert_eq!(
                self.app
                    .world()
                    .resource::<ReferenceEnableState>()
                    .enabled(self.target),
                Some(enabled)
            );
        }

        fn permanent(&mut self) {
            self.app.world_mut().trigger(ReferenceStateChanged {
                reference: self.target,
            });
            self.app.world_mut().flush();
            self.app.update();
            assert!(
                self.app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .contains(self.target)
            );
        }

        fn records(&self, world: u32, grid: IVec2) -> Vec<(RecordKey, u32)> {
            destination_owned_records(
                &self.package,
                &self.sources,
                Some(self.app.world().resource::<RuntimeReferenceState>()),
                world,
                grid,
            )
            .map(|(key, identity)| (*key, identity.reference))
            .collect()
        }

        fn local(&self) -> Vec<(RecordKey, u32)> {
            self.records(self.world, self.grid)
        }
    }

    #[test]
    fn legitimate_disable_removes_only_its_destination_obligations_and_reenable_restores_them() {
        let mut fixture = Fixture::new();
        let initial = fixture.local();
        assert!(
            initial
                .iter()
                .any(|(_, reference)| *reference == fixture.target)
        );
        let siblings: Vec<_> = initial
            .iter()
            .copied()
            .filter(|(_, reference)| *reference == fixture.sibling)
            .collect();
        assert!(!siblings.is_empty());
        fixture.enabled(false);
        let hidden = fixture.local();
        assert!(
            !hidden
                .iter()
                .any(|(_, reference)| *reference == fixture.target),
            "legitimate disabled records must not require a selected cluster bit or stock replacement proof"
        );
        assert_eq!(
            hidden
                .iter()
                .copied()
                .filter(|(_, reference)| *reference == fixture.sibling)
                .collect::<Vec<_>>(),
            siblings
        );
        fixture.enabled(true);
        assert_eq!(
            fixture.local(),
            initial,
            "reenable restores the same certified obligations without inventing replacement records"
        );
    }

    #[test]
    fn permanent_revocation_in_either_disable_order_never_gets_the_legitimate_disable_skip() {
        for disabled_first in [false, true] {
            let mut fixture = Fixture::new();
            let initial = fixture.local();
            if disabled_first {
                fixture.enabled(false);
            }
            fixture.permanent();
            if !disabled_first {
                fixture.enabled(false);
            }
            assert_eq!(
                fixture.local(),
                initial,
                "independent permanent revocation must never be treated as legitimate reversible disable"
            );
            fixture.enabled(true);
            assert!(
                fixture
                    .app
                    .world()
                    .resource::<RuntimeReferenceState>()
                    .contains(fixture.target)
            );
            assert_eq!(
                fixture.local(),
                initial,
                "reenable cannot waive permanently invalid owned geometry"
            );
        }
    }

    #[test]
    fn permanent_material_refusal_remains_excluded_across_runtime_enable_transitions() {
        let mut fixture = Fixture::new();
        let initial = fixture.local();
        let refused: BTreeSet<_> = initial
            .iter()
            .filter(|(_, reference)| *reference == fixture.target)
            .map(|(key, _)| *key)
            .collect();
        assert!(!refused.is_empty());
        fixture
            .sources
            .permanently_refused_records
            .extend(refused.iter().copied());
        let stock_only = fixture.local();
        assert!(!stock_only.iter().any(|(key, _)| refused.contains(key)));
        assert!(
            stock_only
                .iter()
                .any(|(_, reference)| *reference == fixture.sibling)
        );
        for enabled in [false, true] {
            fixture.enabled(enabled);
            assert_eq!(
                fixture.local(),
                stock_only,
                "enable policy never removes the permanent material-refusal exclusion"
            );
        }
    }

    #[test]
    fn runtime_changes_are_reference_local_and_keep_other_placing_worlds_exact() {
        let mut fixture = Fixture::new();
        let unrelated = fixture.records(fixture.other_world, fixture.other_grid);
        assert!(!unrelated.is_empty());
        assert!(
            unrelated
                .iter()
                .all(|(_, reference)| *reference != fixture.target)
        );
        fixture.enabled(false);
        assert_eq!(
            fixture.records(fixture.other_world, fixture.other_grid),
            unrelated
        );
        fixture.permanent();
        assert_eq!(
            fixture.records(fixture.other_world, fixture.other_grid),
            unrelated
        );
        fixture.enabled(true);
        assert_eq!(
            fixture.records(fixture.other_world, fixture.other_grid),
            unrelated
        );
    }

    #[test]
    fn absent_runtime_state_preserves_the_original_certificate_and_refusal_iterator() {
        let mut fixture = Fixture::new();
        let (refused, _) = *fixture.local().first().unwrap();
        fixture.sources.permanently_refused_records.insert(refused);
        let expected: Vec<_> = fixture
            .package
            .records_in_placed_cell(fixture.world, fixture.grid)
            .filter(|(key, _)| **key != refused)
            .map(|(key, identity)| (*key, identity.reference))
            .collect();
        let actual: Vec<_> = destination_owned_records(
            &fixture.package,
            &fixture.sources,
            None,
            fixture.world,
            fixture.grid,
        )
        .map(|(key, identity)| (*key, identity.reference))
        .collect();
        assert_eq!(actual, expected);
    }
}
