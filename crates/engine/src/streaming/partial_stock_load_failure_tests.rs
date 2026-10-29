// Included by partial_stock.rs's tests module.
mod partial_stock_load_failure {
    use super::*;
    use crate::{
        config::EngineConfig,
        streaming::{
            AssetFailure, converted_model_path, error_chain,
            tests::{model_app, settle_readiness, spawn_pending_reference},
        },
        world::components::{CellRef, FormId},
    };

    #[test]
    fn failed_source_load_records_full_and_partial_failures_with_configured_diagnostics() {
        let (_directory, package, row) = fixture();
        // No loader is installed for this extension in model_app. Exercise the
        // actual asynchronous AssetServer failure, not a removed Assets entry.
        let path = format!(
            "{}.partial_stock_load_failure",
            converted_model_path(row.model_path.clone().unwrap()).unwrap()
        );
        for diagnostics in [false, true] {
            let mut app = model_app();
            install(&mut app);
            app.world_mut()
                .resource_mut::<EngineConfig>()
                .diagnostic_asset_fallbacks = diagnostics;
            let asset_server = app.world().resource::<AssetServer>().clone();
            let source = asset_server.load::<WorldAsset>(path.clone());

            // Match Bevy 0.19's asset tests: step the app to an observable
            // terminal state with a finite bound, without sleeps or block_on.
            for _ in 0..10_000 {
                app.update();
                if matches!(
                    asset_server.get_load_state(source.id()),
                    Some(LoadState::Failed(_))
                ) {
                    break;
                }
            }
            let Some(LoadState::Failed(error)) = asset_server.get_load_state(source.id()) else {
                panic!(
                    "source did not fail within 10,000 updates: {:?}",
                    asset_server.get_load_states(source.id())
                );
            };
            assert!(
                !app.world()
                    .resource::<Assets<WorldAsset>>()
                    .contains(source.id())
            );
            let mut dependency_chain = vec![
                format!("REFR {:08X}", row.form_id),
                format!("base record {:08X}", row.base_form_id),
                path.clone(),
            ];
            dependency_chain.extend(error_chain(error.as_ref()));
            let expected_failure = AssetFailure {
                model_path: path.clone(),
                reference_form_id: row.form_id,
                base_form_id: row.base_form_id,
                cell_id: row.cell_id,
                dependency_chain,
            };

            let full = spawn_pending_reference(&mut app, source.clone(), 0);
            let partial = spawn_pending_reference(&mut app, source.clone(), 1);
            for root in [full, partial] {
                app.world_mut().entity_mut(root).insert((
                    FormId(row.form_id),
                    CellRef(row.cell_id),
                    ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::ONE).unwrap(),
                ));
                let mut entity = app.world_mut().entity_mut(root);
                let mut pending = entity.get_mut::<PendingAssetProfile>().unwrap();
                pending.path.clone_from(&path);
                pending.form_id = row.form_id;
                pending.base_form_id = row.base_form_id;
                pending.cell_id = row.cell_id;
                pending.base_record_type.clone_from(&row.base_record_type);
                pending.static_physics = false;
            }
            app.world_mut()
                .entity_mut(partial)
                .insert(PartialStock::candidate(&package, &row).unwrap());

            // Both roots must pass through arm_pending_models and the ordinary
            // readiness scan. The old partial arm removed its profile here,
            // silently losing one failure record and its diagnostic fallback.
            let metrics = settle_readiness(&mut app);
            assert_eq!(metrics.asset_load_failures, 2, "diagnostics={diagnostics}");
            assert_eq!(
                metrics.asset_failures,
                vec![expected_failure.clone(), expected_failure]
            );
            assert_eq!(
                metrics.diagnostic_fallbacks,
                if diagnostics { 2 } else { 0 }
            );
            assert_eq!(metrics.pending_asset_instances, 0);
            assert_eq!(metrics.arming_queue_depth, 0);
            assert_eq!(metrics.assets_ready, 0);
            assert_eq!(metrics.material_validation_failures, 0);
            assert_eq!(metrics.transform_bounds_validation_failures, 0);
            for root in [full, partial] {
                assert!(app.world().get::<AssetFailed>(root).is_some());
                assert!(app.world().get::<PendingModel>(root).is_none());
                assert!(app.world().get::<PendingAssetProfile>(root).is_none());
                assert!(app.world().get::<PartialStock>(root).is_none());
                if root == full {
                    assert_eq!(app.world().get::<WorldAssetRoot>(root).unwrap().0, source);
                } else {
                    assert_ne!(app.world().get::<WorldAssetRoot>(root).unwrap().0, source);
                    assert_eq!(
                        app.world().get::<OriginalStockScene>(root).unwrap().0,
                        source
                    );
                }
                let mut fallbacks =
                    app.world_mut()
                        .query::<(&ChildOf, &Name, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
                let count = fallbacks
                    .iter(app.world())
                    .filter(|(parent, name, _, _)| {
                        parent.parent() == root && name.as_str() == "DIAGNOSTIC ASSET FAILURE"
                    })
                    .count();
                assert_eq!(count, usize::from(diagnostics));
            }

            // A settled failed placement must not be counted again next frame.
            let settled = settle_readiness(&mut app);
            assert_eq!(settled.asset_load_failures, metrics.asset_load_failures);
            assert_eq!(settled.asset_failures, metrics.asset_failures);
            assert_eq!(settled.diagnostic_fallbacks, metrics.diagnostic_fallbacks);
        }
    }
}
