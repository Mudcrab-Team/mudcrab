// Ordinary behavior and selected admission are checked through their real owners.

mod admission_compatibility {
    use super::*;

    #[test]
    fn streaming_installs_admission_owners_only_for_selected_geometry() {
        for selected in [false, true] {
            let mut app = App::new();
            app.insert_resource(EngineConfig {
                clusters: selected.then(|| "fixture".to_owned()),
                ..default()
            });
            app.add_plugins(StreamingPlugin);
            assert_eq!(
                app.world()
                    .contains_resource::<crate::world::reference_state::ReferenceEnableState>(),
                selected
            );
            assert_eq!(
                app.world()
                    .contains_resource::<crate::cluster::ownership::OwnershipFacts>(),
                selected
            );
            assert_eq!(
                app.world()
                    .contains_resource::<crate::cluster::ownership::OwnershipBatchDemand>(),
                selected
            );
            if selected {
                assert!(
                    !app.world()
                        .resource::<crate::world::reference_state::ReferenceEnableState>()
                        .is_bound()
                );
                app.world_mut()
                    .trigger(crate::world::reference_state::SetReferenceEnabled {
                        reference: 17,
                        enabled: false,
                    });
                app.world_mut().flush();
                assert!(
                    !app.world()
                        .resource::<crate::world::reference_state::ReferenceEnableState>()
                        .is_bound()
                );
            }
        }
    }
}
