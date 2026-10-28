use super::*;
use bevy_asset::AssetId;
use bevy_ecs::entity::EntityIndex;

fn entity(index: u32) -> Entity {
    Entity::from_index(EntityIndex::from_raw_u32(index).unwrap())
}

fn view(index: u32) -> RetainedViewEntity {
    RetainedViewEntity::new(entity(1).into(), Some(entity(index).into()), 0)
}

fn key() -> MapKey {
    MapKey {
        entity: entity(5),
        inputs: DirectionalShadowMapInputs {
            texture: TextureId::new(),
            slice: 0,
            matrices: [[0; 16]; 7],
            light: [0; 9],
            camera_layers: Default::default(),
            light_layers: Default::default(),
        },
        shadow_view: ShadowViewInputs::default(),
        external: 1,
        external_materials: 1,
        stock: (1, 1),
        geometry: 1,
        meshes: 1,
        images: 1,
        pipelines: 1,
        draw_producers: (0, 0, 0, 0),
    }
}

fn cache() -> ShadowMapCache {
    ShadowMapCache(Mutex::default())
}

fn begin(cache: &ShadowMapCache, view: RetainedViewEntity, frame: u32, key: MapKey) -> bool {
    let allocation = (key.inputs.texture, key.inputs.slice);
    let producers = key.draw_producers;
    cache.begin_key(
        view,
        frame,
        Ok(key),
        Some(allocation),
        Ok(producers),
        "cascade",
    )
}

fn rendered(cache: &ShadowMapCache, view: RetainedViewEntity, key: MapKey) {
    assert!(!begin(cache, view, 1, key));
    cache.finish(view, false, false, true);
}

mod view_windows {
    use super::*;

    #[test]
    fn failure_after_window_close_remains_pending_without_another_view_frame() {
        let cache = cache();
        let view = view(2);
        rendered(&cache, view, key());
        close(&cache, view, 1);
        assert!(!lock(&cache.0).attempts[&view].window.has_pending());
        cache.incomplete(view);
        assert!(lock(&cache.0).attempts[&view].window.has_pending());
        let report = close(&cache, view, 2);
        assert_eq!(report.observed_frames, 0);
        assert_eq!(report.counters.incomplete_draw, 1);
        assert!(!report.zero_render_proof());
    }

    fn close(
        cache: &ShadowMapCache,
        view: RetainedViewEntity,
        frame: u32,
    ) -> ShadowViewWindowReport {
        lock(&cache.0)
            .attempts
            .get_mut(&view)
            .unwrap()
            .window
            .close(frame, true)
    }

    #[test]
    fn one_changed_view_does_not_hide_other_views_zero_render_windows() {
        let cache = cache();
        let keys = [key(), key(), key()];
        for (index, key) in keys.iter().enumerate() {
            let view = view(index as u32 + 2);
            rendered(&cache, view, key.clone());
            close(&cache, view, 1);
        }
        for frame in 2..=301 {
            for (index, key) in keys.iter().enumerate() {
                let mut key = key.clone();
                if index == 0 {
                    key.external = u64::from(frame);
                }
                let view = view(index as u32 + 2);
                assert_eq!(begin(&cache, view, frame, key), index != 0);
                cache.finish(view, false, false, true);
            }
        }
        assert_eq!(cache.counters().rendered, 303);
        let changed = close(&cache, view(2), 301);
        assert_eq!(
            (changed.counters.rendered, changed.counters.invalidated),
            (300, 300)
        );
        assert!(!changed.zero_render_proof());
        for view in [view(3), view(4)] {
            let quiet = close(&cache, view, 301);
            assert_eq!((quiet.counters.reused, quiet.counters.rendered), (300, 0));
            assert!(quiet.zero_render_proof());
        }
    }

    #[test]
    fn stable_labels_keep_storage_and_windows_reset_without_old_counts() {
        let cache = cache();
        let key = key();
        rendered(&cache, view(2), key.clone());
        let label = lock(&cache.0).attempts[&view(2)].window.pass_name.as_ptr();
        let first = close(&cache, view(2), 1);
        assert_eq!(
            (first.start_frame, first.end_frame, first.counters.rendered),
            (1, 1, 1)
        );
        assert!(begin(&cache, view(2), 2, key.clone()));
        let second = close(&cache, view(2), 2);
        assert_eq!(
            (
                second.start_frame,
                second.observed_frames,
                second.counters.rendered
            ),
            (2, 1, 0)
        );
        assert!(second.zero_render_proof());
        assert!(begin(&cache, view(2), 3, key));
        assert_eq!(
            lock(&cache.0).attempts[&view(2)].window.pass_name.as_ptr(),
            label
        );
        assert_eq!(close(&cache, view(2), 3).counters.reused, 1);
    }

    #[test]
    fn changed_labels_refuse_zero_proof_until_next_stable_window() {
        let cache = cache();
        let key = key();
        rendered(&cache, view(2), key.clone());
        close(&cache, view(2), 1);
        assert!(cache.begin_key(
            view(2),
            2,
            Ok(key.clone()),
            Some((key.inputs.texture, key.inputs.slice)),
            Ok(key.draw_producers),
            "cascade_changed"
        ));
        let changed = close(&cache, view(2), 2);
        assert_eq!(changed.counters.rendered, 0);
        assert_eq!(changed.label_changes, 1);
        assert!(!changed.label_valid);
        assert!(!changed.zero_render_proof());
        assert!(cache.begin_key(
            view(2),
            3,
            Ok(key.clone()),
            Some((key.inputs.texture, key.inputs.slice)),
            Ok(key.draw_producers),
            "cascade_changed"
        ));
        assert!(close(&cache, view(2), 3).zero_render_proof());
    }

    #[test]
    fn late_producer_revocation_preserves_old_map_and_counts_refusal() {
        for producers in [Err(Refusal::UnsupportedProducer), Ok((1, 0, 0, 0))] {
            let cache = cache();
            let key = key();
            rendered(&cache, view(2), key.clone());
            close(&cache, view(2), 1);
            assert!(begin(&cache, view(2), 2, key));
            assert!(!cache.validate_draw_producers(view(2), producers));
            assert!(!cache.validate_draw_producers(view(2), producers));
            cache.finish(view(2), true, true, false);
            let report = close(&cache, view(2), 2);
            assert_eq!(
                (
                    report.counters.reused,
                    report.counters.rendered,
                    report.counters.incomplete_draw
                ),
                (1, 0, 1)
            );
            assert_eq!(
                report.counters.unsupported_producer + report.counters.invalidated,
                2
            );
            assert!(!report.zero_render_proof());
        }
    }

    #[test]
    fn refusal_and_phase_failure_are_owned_by_the_view() {
        for refusal in [
            Refusal::External,
            Refusal::UnknownMaterial,
            Refusal::UnsupportedProducer,
            Refusal::PendingStock,
            Refusal::UntrackedTexture,
        ] {
            let cache = cache();
            assert!(!cache.begin_key(view(2), 1, Err(refusal), None, Ok((0, 0, 0, 0)), "cascade"));
            cache.finish(view(2), false, false, false);
            let report = close(&cache, view(2), 1);
            let c = report.counters;
            assert_eq!(
                c.external_incomplete
                    + c.unknown_material
                    + c.unsupported_producer
                    + c.stock_pending
                    + c.untracked_texture,
                1
            );
            assert_eq!((c.rendered, c.incomplete_draw), (1, 1));
            assert!(!report.zero_render_proof());
        }
    }

    #[test]
    fn missing_frames_and_lifecycle_boundaries_refuse_zero_proof() {
        let cache = cache();
        let key = key();
        rendered(&cache, view(2), key.clone());
        close(&cache, view(2), 1);
        assert!(begin(&cache, view(2), 3, key.clone()));
        assert!(!close(&cache, view(2), 3).zero_render_proof());
        assert!(begin(&cache, view(2), 4, key.clone()));
        let terminal = lock(&cache.0)
            .attempts
            .get_mut(&view(2))
            .unwrap()
            .window
            .close(4, false);
        assert!(!terminal.zero_render_proof());
        cache.invalidate_physical_maps();
        assert!(lock(&cache.0).attempts.is_empty());
        assert!(!begin(&cache, view(2), 5, key.clone()));
        assert_eq!(close(&cache, view(2), 5).counters.rendered, 1);
        cache.retain(&HashSet::default());
        assert!(!begin(&cache, view(2), 6, key));
        assert_eq!(close(&cache, view(2), 6).counters.rendered, 1);
    }
}

mod draw_producer_regressions {
    use super::*;
    use bevy_render::render_phase::{Draw, DrawError};

    // Represents a NonMesh producer whose uniforms live outside the stock
    // mesh/material producers. No GPU is needed to exercise the cache gate.
    struct UnknownDraw;

    impl Draw<Shadow> for UnknownDraw {
        fn draw<'w>(
            &mut self,
            _: &'w World,
            _: &mut TrackedRenderPass<'w>,
            _: Entity,
            _: &Shadow,
        ) -> Result<(), DrawError> {
            unreachable!("cache admission tests do not rasterize")
        }
    }

    struct ExternalCommand<const N: usize = 0>;

    impl<const N: usize> RenderCommand<Shadow> for ExternalCommand<N> {
        type Param = ();
        type ViewQuery = ();
        type ItemQuery = ();

        fn render<'w>(
            _: &Shadow,
            _: ROQueryItem<'w, '_, Self::ViewQuery>,
            _: Option<ROQueryItem<'w, '_, Self::ItemQuery>>,
            _: SystemParamItem<'w, '_, Self::Param>,
            _: &mut TrackedRenderPass<'w>,
        ) -> RenderCommandResult {
            RenderCommandResult::Skip
        }
    }

    fn add_external(draws: &DrawFunctions<Shadow>) {
        add_external_command::<0>(draws);
    }

    fn add_external_command<const N: usize>(draws: &DrawFunctions<Shadow>) {
        let mut world = World::new();
        world.insert_resource(cache());
        let command =
            RenderCommandState::<Shadow, ShadowCacheTracking<ExternalCommand<N>>>::new(&mut world);
        draws
            .write()
            .add_with::<ShadowCacheTracking<ExternalCommand<N>>, _>(command);
    }

    fn begin_registered(
        cache: &ShadowMapCache,
        draws: &DrawFunctions<Shadow>,
        external: &ShadowMapExternalInputs,
        frame: u32,
        key: &MapKey,
    ) -> bool {
        cache.begin_key(
            view(2),
            frame,
            Ok(key.clone()),
            Some((key.inputs.texture, key.inputs.slice)),
            shadow_draw_producers(draws, external, view(2), frame),
            "cascade",
        )
    }

    #[test]
    fn unknown_nonmesh_insertion_refuses_previously_settled_map_in_same_frame() {
        let cache = cache();
        let key = key();
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.finish(view(2), false, false, true);
        assert!(begin_registered(&cache, &draws, &external, 2, &key));
        draws.write().add(UnknownDraw);
        assert!(!begin_registered(&cache, &draws, &external, 2, &key));
        assert_eq!(cache.counters().unsupported_producer, 1);
    }

    #[test]
    fn unknown_removal_must_render_before_reuse_can_recover() {
        let cache = cache();
        let key = key();
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.finish(view(2), false, false, true);
        draws.write().add(UnknownDraw);
        assert!(!begin_registered(&cache, &draws, &external, 2, &key));
        cache.finish(view(2), false, false, true);
        draws.write().clear();
        assert!(!begin_registered(&cache, &draws, &external, 2, &key));
        cache.finish(view(2), false, false, true);
        assert!(begin_registered(&cache, &draws, &external, 3, &key));
    }

    #[test]
    fn aggregate_certificate_cannot_authorize_unknown_external_uniform_inputs() {
        let cache = cache();
        let key = key();
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        draws.write().add(UnknownDraw);
        external.publish(view(2), 1, 7, true);
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.finish(view(2), false, false, true);
        // The aggregate cluster certificate remains unchanged while a
        // custom producer changes a uniform that it alone owns.
        external.publish(view(2), 2, 7, true);
        assert!(!begin_registered(&cache, &draws, &external, 2, &key));
        assert_eq!(cache.counters().unsupported_producer, 2);
    }

    #[test]
    fn stock_registration_alias_cannot_hide_a_custom_draw() {
        let draws = DrawFunctions::default();
        draws
            .write()
            .add_with::<ShadowCacheTracking<crate::DrawPrepass>, _>(UnknownDraw);
        assert_eq!(
            shadow_draw_producers(&draws, &ShadowMapExternalInputs::default(), view(2), 1),
            Err(Refusal::UnsupportedProducer),
        );
    }

    #[test]
    fn wrapped_external_draw_still_requires_its_own_current_complete_certificate() {
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        add_external(&draws);
        external.publish(view(2), 1, 7, true);
        assert_eq!(
            shadow_draw_producers(&draws, &external, view(2), 1),
            Err(Refusal::UnsupportedProducer)
        );
        external.publish_draw::<ExternalCommand>(view(2), 1, 9, false);
        assert_eq!(
            shadow_draw_producers(&draws, &external, view(2), 1),
            Err(Refusal::UnsupportedProducer)
        );
        external.publish_draw::<ExternalCommand>(view(2), 1, 9, true);
        assert!(shadow_draw_producers(&draws, &external, view(2), 1).is_ok());
        assert_eq!(
            shadow_draw_producers(&draws, &external, view(2), 2),
            Err(Refusal::UnsupportedProducer)
        );
        assert_eq!(
            shadow_draw_producers(&draws, &external, view(3), 1),
            Err(Refusal::UnsupportedProducer)
        );
    }

    #[test]
    fn certified_external_input_change_and_removal_invalidate_in_same_frame() {
        let cache = cache();
        let key = key();
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        add_external(&draws);
        external.publish_draw::<ExternalCommand>(view(2), 1, 9, true);
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.finish(view(2), false, false, true);
        external.publish_draw::<ExternalCommand>(view(2), 2, 9, true);
        assert!(begin_registered(&cache, &draws, &external, 2, &key));
        external.publish_draw::<ExternalCommand>(view(2), 2, 10, true);
        assert!(!begin_registered(&cache, &draws, &external, 2, &key));
        cache.finish(view(2), false, false, true);
        draws.write().clear();
        assert!(!begin_registered(&cache, &draws, &external, 2, &key));
    }

    #[test]
    fn direct_registry_replacement_is_not_hidden_by_unchanged_length_or_aliases() {
        let cache = cache();
        let key = key();
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        add_external(&draws);
        external.publish_draw::<ExternalCommand>(view(2), 1, 9, true);
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.finish(view(2), false, false, true);
        assert!(draws
            .write()
            .replace(bevy_render::render_phase::DrawFunctionId(0), UnknownDraw));
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
    }

    #[test]
    fn registration_after_early_reuse_invalidates_late_pass_and_physical_owner() {
        let cache = cache();
        let key = key();
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.finish(view(2), false, false, true);
        assert!(begin_registered(&cache, &draws, &external, 2, &key));
        draws.write().add(UnknownDraw);
        assert!(!cache.validate_draw_producers(
            view(2),
            shadow_draw_producers(&draws, &external, view(2), 2)
        ));
        assert!(lock(&cache.0).owners.is_empty());
        let mut world = World::new();
        world.insert_resource(FrameCount(2));
        world.insert_resource(draws);
        world.insert_resource(external);
        // The early clear/draw was omitted. The actual late entry point
        // must also omit rendering, then force a full clear next frame.
        assert!(cache.begin(&world, entity(5), view(2), true, "cascade"));
        cache.finish(view(2), true, true, true);
        assert!(lock(&cache.0).owners.is_empty());
        assert!(!begin(&cache, view(2), 3, key));
    }

    #[test]
    fn certified_skip_never_commits_a_map() {
        let cache = cache();
        let key = key();
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        add_external(&draws);
        external.publish_draw::<ExternalCommand>(view(2), 1, 9, true);
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.command_result(view(2), &RenderCommandResult::Skip);
        cache.finish(view(2), false, false, true);
        external.publish_draw::<ExternalCommand>(view(2), 2, 9, true);
        assert!(!begin_registered(&cache, &draws, &external, 2, &key));
        assert_eq!(cache.counters().incomplete_draw, 1);
    }

    #[test]
    fn settled_registry_checks_do_not_reclassify_after_unchanged_publications() {
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        add_external(&draws);
        external.publish_draw::<ExternalCommand>(view(2), 1, 9, true);
        let baseline = shadow_draw_producers(&draws, &external, view(2), 1).unwrap();
        for frame in 2..20 {
            external.publish_draw::<ExternalCommand>(view(2), frame, 9, true);
            assert_eq!(
                shadow_draw_producers(&draws, &external, view(2), frame).unwrap(),
                baseline
            );
        }
        assert_eq!(lock(&external.2).classifications, 1);
    }

    #[test]
    fn registry_recreation_changes_the_revision_even_with_identical_contents() {
        let cache = cache();
        let key = key();
        let external = ShadowMapExternalInputs::default();
        let draws = DrawFunctions::default();
        assert!(!begin_registered(&cache, &draws, &external, 1, &key));
        cache.finish(view(2), false, false, true);
        let replacement = DrawFunctions::default();
        assert!(!begin_registered(&cache, &replacement, &external, 1, &key));
    }

    #[test]
    fn preparing_and_fetching_draws_preserve_the_registry_revision() {
        let draws = DrawFunctions::default();
        let mut functions = draws.write();
        let id = functions.add(UnknownDraw);
        let revision = functions.draw_functions.revision();
        functions.prepare(&World::new());
        assert!(functions.get_mut(id).is_some());
        assert_eq!(functions.draw_functions.revision(), revision);
    }

    #[test]
    fn third_external_command_cannot_expand_the_certified_pair() {
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        external.publish_draw::<ExternalCommand<0>>(view(2), 1, 9, true);
        external.publish_draw::<ExternalCommand<1>>(view(2), 1, 9, true);
        external.publish_draw::<ExternalCommand<2>>(view(2), 1, 9, true);
        add_external_command::<2>(&draws);
        assert_eq!(
            shadow_draw_producers(&draws, &external, view(2), 1),
            Err(Refusal::UnsupportedProducer)
        );
    }

    #[test]
    fn external_certificate_alias_cannot_authorize_a_raw_draw() {
        let draws = DrawFunctions::default();
        let external = ShadowMapExternalInputs::default();
        external.publish_draw::<ExternalCommand>(view(2), 1, 9, true);
        draws
            .write()
            .add_with::<ShadowCacheTracking<ExternalCommand>, _>(UnknownDraw);
        assert_eq!(
            shadow_draw_producers(&draws, &external, view(2), 1),
            Err(Refusal::UnsupportedProducer)
        );
    }
}

#[test]
fn settled_map_skips_both_clear_and_render() {
    let cache = cache();
    let key = key();
    let mut clears = 0;
    let mut draws = 0;
    for frame in 1..5 {
        if !begin(&cache, view(2), frame, key.clone()) {
            clears += 1;
            draws += 1;
            cache.finish(view(2), false, false, true);
        }
    }
    assert_eq!((clears, draws, cache.counters().reused), (1, 1, 3));
}

#[test]
fn every_exact_scalar_input_invalidates_in_the_same_frame() {
    let baseline = key();
    // Covers all seven matrices and nine light/LOD/bias/extent words.
    for input in 0..(7 * 16 + 9) {
        let cache = cache();
        rendered(&cache, view(2), baseline.clone());
        let mut changed = baseline.clone();
        *changed
            .inputs
            .matrices
            .iter_mut()
            .flatten()
            .chain(changed.inputs.light.iter_mut())
            .nth(input)
            .unwrap() ^= 1;
        assert!(!begin(&cache, view(2), 2, changed), "input word {input}");
    }
}

#[test]
fn every_exact_shadow_view_word_invalidates_in_the_same_frame() {
    let baseline = key();
    // Together with prepare_lights' 121 words, these cover all 177 words.
    for input in 0..(4 + 3 * 16 + 1 + 3) {
        let cache = cache();
        rendered(&cache, view(2), baseline.clone());
        let mut changed = baseline.clone();
        let shadow_view = &mut changed.shadow_view;
        *shadow_view
            .viewport
            .iter_mut()
            .chain(shadow_view.world_from_view.iter_mut())
            .chain(shadow_view.clip_from_view.iter_mut())
            .chain(shadow_view.clip_from_world.iter_mut())
            .chain(core::iter::once(&mut shadow_view.mip_bias))
            .chain(shadow_view.lod_origin.iter_mut())
            .nth(input)
            .unwrap() ^= 1;
        assert!(
            !begin(&cache, view(2), 2, changed),
            "shadow-view word {input}"
        );
    }
}

#[test]
fn every_producer_revision_invalidates_independently() {
    let baseline = key();
    for field in 0..10 {
        let cache = cache();
        rendered(&cache, view(2), baseline.clone());
        let mut changed = baseline.clone();
        match field {
            0 => changed.external += 1,
            1 => changed.external_materials += 1,
            2 => changed.stock.0 += 1,
            3 => changed.stock.1 += 1,
            4 => changed.geometry += 1,
            5 => changed.meshes += 1,
            6 => changed.images += 1,
            7 => changed.pipelines += 1,
            8 => changed.entity = entity(6),
            _ => changed.draw_producers.0 += 1,
        }
        assert!(
            !begin(&cache, view(2), 2, changed),
            "producer field {field}"
        );
    }
}

#[test]
fn camera_and_light_layers_invalidate_independently() {
    for camera in [true, false] {
        let baseline = key();
        let cache = cache();
        rendered(&cache, view(2), baseline.clone());
        let mut changed = baseline;
        if camera {
            changed.inputs.camera_layers = bevy_camera::visibility::RenderLayers::layer(1);
        } else {
            changed.inputs.light_layers = bevy_camera::visibility::RenderLayers::layer(1);
        }
        assert!(!begin(&cache, view(2), 2, changed));
    }
}

#[test]
fn physical_allocation_and_slice_changes_invalidate() {
    for reallocate in [true, false] {
        let baseline = key();
        let cache = cache();
        rendered(&cache, view(2), baseline.clone());
        let mut changed = baseline;
        if reallocate {
            changed.inputs.texture = TextureId::new();
        } else {
            changed.inputs.slice += 1;
        }
        assert!(!begin(&cache, view(2), 2, changed));
    }
}

#[test]
fn another_camera_overwriting_a_slice_prevents_reuse() {
    let cache = cache();
    let key = key();
    rendered(&cache, view(2), key.clone());
    rendered(&cache, view(3), key.clone());
    assert!(!begin(&cache, view(2), 2, key));
}

#[test]
fn removed_and_recreated_views_cannot_reuse_old_maps() {
    let cache = cache();
    let key = key();
    rendered(&cache, view(2), key.clone());
    cache.retain(&HashSet::default());
    assert!(!begin(&cache, view(2), 2, key));
}

#[test]
fn incomplete_commands_never_commit_and_ready_retry_can_recover() {
    let cache = cache();
    let key = key();
    assert!(!begin(&cache, view(2), 1, key.clone()));
    cache.incomplete(view(2)); // RenderCommandResult::Skip or Failure.
    cache.finish(view(2), false, false, true); // phase render returns Ok on Skip.
    assert!(!begin(&cache, view(2), 2, key.clone()));
    cache.finish(view(2), false, false, true);
    assert!(begin(&cache, view(2), 3, key));
}

#[test]
fn occlusion_requires_successful_early_and_late_passes() {
    let cache = cache();
    let key = key();
    assert!(!begin(&cache, view(2), 1, key.clone()));
    cache.finish(view(2), false, true, true);
    assert!(lock(&cache.0).owners.is_empty());
    cache.incomplete(view(2));
    cache.finish(view(2), true, true, true);
    assert!(lock(&cache.0).owners.is_empty());
    assert!(!begin(&cache, view(2), 2, key.clone()));
    cache.finish(view(2), false, true, true);
    cache.finish(view(2), true, true, true);
    assert!(begin(&cache, view(2), 3, key));
}

#[test]
fn refusals_invalidate_previous_physical_owner() {
    for refusal in [
        Refusal::External,
        Refusal::UnknownMaterial,
        Refusal::UnsupportedProducer,
        Refusal::PendingStock,
        Refusal::UntrackedTexture,
    ] {
        let cache = cache();
        let key = key();
        rendered(&cache, view(2), key.clone());
        assert!(!cache.begin_key(
            view(2),
            2,
            Err(refusal),
            Some((key.inputs.texture, key.inputs.slice)),
            Ok((0, 0, 0, 0)),
            "cascade",
        ));
        cache.finish(view(2), false, false, true);
        assert!(!begin(&cache, view(2), 3, key));
    }
}

fn material() -> UntypedAssetId {
    AssetId::<StandardMaterial>::default().untyped()
}

#[test]
fn caster_mutations_only_invalidate_views_that_use_them() {
    let inputs = ShadowStockInputs::default();
    inputs.set_caster(
        view(2),
        entity(10).into(),
        material(),
        Some(1),
        Some(0),
        false,
    );
    inputs.set_caster(
        view(3),
        entity(11).into(),
        material(),
        Some(1),
        Some(0),
        false,
    );
    let before = (
        inputs.snapshot(view(2)).unwrap(),
        inputs.snapshot(view(3)).unwrap(),
    );
    inputs.caster_changed(entity(10).into());
    assert_ne!(inputs.snapshot(view(2)).unwrap(), before.0);
    assert_eq!(inputs.snapshot(view(3)).unwrap(), before.1);
}

#[test]
fn stock_removal_and_morph_changes_invalidate_without_population_scans() {
    let inputs = ShadowStockInputs::default();
    inputs.set_caster(
        view(2),
        entity(10).into(),
        material(),
        Some(1),
        Some(0),
        false,
    );
    inputs.observe_morph(entity(10).into(), &[0.25], true);
    let before = inputs.snapshot(view(2)).unwrap();
    inputs.observe_morph(entity(10).into(), &[0.25], false);
    assert_eq!(inputs.snapshot(view(2)).unwrap(), before);
    inputs.observe_morph(entity(10).into(), &[0.5], true);
    assert_ne!(inputs.snapshot(view(2)).unwrap(), before);
    let before = inputs.snapshot(view(2)).unwrap();
    inputs.remove_caster(view(2), entity(10).into());
    assert_ne!(inputs.snapshot(view(2)).unwrap(), before);
}

#[test]
fn morph_producer_ticks_gate_byte_inspection_and_reappearance() {
    let inputs = ShadowStockInputs::default();
    let caster = entity(10).into();
    inputs.set_caster(view(2), caster, material(), Some(1), Some(0), false);
    inputs.observe_morph(caster, &[0.25], true);
    let before = inputs.snapshot(view(2)).unwrap();
    // A settled producer certificate prevents reading the supplied bytes.
    // Actual ECS writes always mark either the value or reference changed.
    inputs.observe_morph(caster, &[0.5], false);
    assert_eq!(inputs.snapshot(view(2)).unwrap(), before);
    assert_eq!(lock(&inputs.0).morph_weights[&caster], [0.25f32.to_bits()]);
    inputs.observe_morph(caster, &[0.5], true);
    assert_ne!(inputs.snapshot(view(2)).unwrap(), before);
    inputs.remove_morph(caster);
    inputs.observe_morph(caster, &[0.75], false);
    assert_eq!(lock(&inputs.0).morph_weights[&caster], [0.75f32.to_bits()]);
}

#[test]
fn unrelated_material_mutations_do_not_touch_selected_cascade_revisions() {
    let inputs = ShadowStockInputs::default();
    inputs.set_caster(
        view(2),
        entity(10).into(),
        material(),
        Some(1),
        Some(0),
        false,
    );
    let before = inputs.snapshot(view(2)).unwrap();
    let unrelated = AssetId::<StandardMaterial>::invalid().untyped();
    route_material_change(&mut lock(&inputs.0), unrelated, Some((9, 3)), false);
    assert_eq!(inputs.snapshot(view(2)).unwrap(), before);
    route_material_change(&mut lock(&inputs.0), material(), Some((2, 0)), false);
    assert_ne!(inputs.snapshot(view(2)).unwrap(), before);
}

#[test]
fn opaque_texture_bindings_refuse_until_inspectable_material_replacement() {
    let inputs = ShadowStockInputs::default();
    inputs.set_caster(
        view(2),
        entity(10).into(),
        material(),
        Some(1),
        Some(0),
        true,
    );
    assert_eq!(inputs.snapshot(view(2)), Err(Refusal::UntrackedTexture));
    route_material_change(&mut lock(&inputs.0), material(), Some((2, 0)), false);
    assert!(inputs.snapshot(view(2)).is_ok());
}

#[test]
fn external_closure_survives_settled_frames_and_receives_late_material_mutations() {
    let external = ShadowMapExternalInputs::default();
    external.reset_materials(view(2), 1);
    {
        let mut inputs = lock(&external.1);
        inputs
            .views
            .get_mut(&view(2))
            .unwrap()
            .materials
            .insert(material());
        inputs
            .material_views
            .entry(material())
            .or_default()
            .insert(view(2));
        route_external_material_change(&mut inputs, material(), Some((1, 0)), false);
    }
    external.begin_materials(view(2), 2);
    let before = lock(&external.1).views[&view(2)].revision;
    assert!(lock(&external.1).views[&view(2)]
        .materials
        .contains(&material()));
    route_external_material_change(&mut lock(&external.1), material(), Some((2, 0)), true);
    assert!(lock(&external.1).views[&view(2)]
        .untracked
        .contains(&material()));
    assert_ne!(lock(&external.1).views[&view(2)].revision, before);
    external.reset_materials(view(2), 3);
    assert!(lock(&external.1).views[&view(2)].untracked.is_empty());
    assert!(!lock(&external.1).material_views.contains_key(&material()));
}

#[test]
fn repeated_empty_external_closures_preserve_revision_and_transitions_invalidate() {
    let external = ShadowMapExternalInputs::default();
    external.reset_materials(view(2), 1);
    let empty_revision = lock(&external.1).views[&view(2)].revision;
    for frame in 2..10 {
        external.reset_materials(view(2), frame);
        let inputs = lock(&external.1);
        let state = &inputs.views[&view(2)];
        assert_eq!(state.frame, frame);
        assert!(state.initialized);
        assert_eq!(
            state.revision, empty_revision,
            "unchanged empty frame {frame}"
        );
    }
    // Mirror the existing queue's membership publication, without needing
    // a GPU PreparedMaterial to exercise closure lifetime and invalidation.
    {
        let mut inputs = lock(&external.1);
        let state = inputs.views.get_mut(&view(2)).unwrap();
        state.materials.insert(material());
        inputs
            .material_views
            .entry(material())
            .or_default()
            .insert(view(2));
        route_external_material_change(&mut inputs, material(), Some((1, 0)), false);
    }
    let populated_revision = lock(&external.1).views[&view(2)].revision;
    assert_ne!(populated_revision, empty_revision);
    external.reset_materials(view(2), 10);
    let cleared_revision = lock(&external.1).views[&view(2)].revision;
    assert_ne!(cleared_revision, populated_revision);
    assert!(!lock(&external.1).material_views.contains_key(&material()));
    external.reset_materials(view(2), 11);
    assert_eq!(lock(&external.1).views[&view(2)].revision, cleared_revision);
}

#[cfg(test)]
mod texture_usage_tests {
    use super::gpu_texture_contents_untracked;
    use bevy_render::render_resource::TextureUsages;

    #[test]
    fn managed_uploads_remain_eligible_but_gpu_written_bindings_refuse() {
        let managed = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
        assert!(!gpu_texture_contents_untracked(managed));
        for writable in [
            TextureUsages::RENDER_ATTACHMENT,
            TextureUsages::STORAGE_BINDING,
        ] {
            assert!(gpu_texture_contents_untracked(managed | writable));
        }
    }
}

#[test]
fn two_active_cameras_keep_reusing_their_own_cascades() {
    let cache = cache();
    let baseline = key();
    for frame in 1..=4 {
        for camera in 0..2 {
            for cascade in 0..4 {
                let view = RetainedViewEntity::new(
                    entity(1).into(),
                    Some(entity(2 + camera).into()),
                    cascade,
                );
                let mut key = baseline.clone();
                key.entity = entity(10 + camera * 4 + cascade);
                key.inputs.slice = camera * 4 + cascade;
                assert_eq!(begin(&cache, view, frame, key), frame > 1);
                if frame == 1 {
                    cache.finish(view, false, false, true);
                }
            }
        }
    }
    assert_eq!(cache.counters().rendered, 8);
    assert_eq!(cache.counters().reused, 24);
    assert_eq!(lock(&cache.0).owners.len(), 8);
}

fn poison<T>(mutex: &Mutex<T>) {
    let result = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        let _guard = mutex.lock().unwrap();
        panic!("injected incomplete dependency publication");
    }));
    assert!(result.is_err());
    assert!(mutex.is_poisoned());
}

#[test]
fn poisoned_maps_discard_owners_and_never_reuse_partial_state() {
    let cache = cache();
    let key = key();
    rendered(&cache, view(2), key.clone());
    poison(&cache.0);
    for frame in 2..=3 {
        assert!(!begin(&cache, view(2), frame, key.clone()));
        cache.finish(view(2), false, false, true);
        assert!(lock(&cache.0).owners.is_empty());
        assert!(!cache.0.is_poisoned());
        assert!(lock(&cache.0).retention_disabled);
    }
}

#[test]
fn poisoned_dependencies_refuse_instead_of_reading_the_old_certificate() {
    let stock = ShadowStockInputs::default();
    stock.begin_view(view(2));
    poison(&stock.0);
    stock.begin_view(view(2)); // Mutation drops poisoned state; never cures poison.
    assert_eq!(stock.snapshot(view(2)), Err(Refusal::PendingStock));
    let external = ShadowMapExternalInputs::default();
    poison(&external.2);
    assert_eq!(
        shadow_draw_producers(&DrawFunctions::default(), &external, view(2), 1),
        Err(Refusal::UnsupportedProducer)
    );
    for material_state in [false, true] {
        let mut world = World::new();
        let external = ShadowMapExternalInputs::default();
        external.publish(view(2), 1, 7, true);
        if material_state {
            poison(&external.1);
        } else {
            poison(&external.0);
        }
        world.insert_resource(external);
        assert!(matches!(
            make_key(&world, entity(5), view(2), 1),
            Err(Refusal::External)
        ));
    }
}

#[test]
fn many_point_views_do_zero_stock_bookkeeping() {
    use core::sync::atomic::Ordering;
    const POINT_VIEWS: u32 = 30 * 6;
    const DIRECTIONAL_VIEWS: u32 = 4;
    const CASTERS: u32 = 10_000;
    // Fixture IDs are test data, independent of any engine/world content IDs.
    let directional = key().inputs;
    #[derive(Debug)]
    struct Measurement {
        locks: u64,
        membership_attempts: u64,
        retained_views: usize,
        retained_memberships: usize,
        point_locks: u64,
        point_membership_attempts: u64,
    }
    fn measure(gated: bool, directional: &DirectionalShadowMapInputs) -> Measurement {
        let stock = ShadowStockInputs::default();
        let casters: Vec<MainEntity> = (1..=CASTERS).map(|index| entity(index).into()).collect();
        let mut point_locks = 0;
        let mut point_membership_attempts = 0;
        for index in 0..POINT_VIEWS + DIRECTIONAL_VIEWS {
            let view = view(20_000 + index);
            let inputs = if gated {
                stock.for_directional_view((index >= POINT_VIEWS).then_some(directional))
            } else {
                Some(&stock)
            };
            if let Some(inputs) = inputs {
                inputs.begin_view(view);
                for &caster in &casters {
                    inputs.remove_caster(view, caster);
                    inputs.set_caster(view, caster, material(), Some(1), Some(0), false);
                }
                inputs.refuse_view(view); // Pending specialization publication.
            }
            if index + 1 == POINT_VIEWS {
                point_locks = stock.1.mutation_locks.load(Ordering::Relaxed);
                point_membership_attempts = stock.1.caster_memberships.load(Ordering::Relaxed);
            }
        }
        let state = lock(&stock.0);
        Measurement {
            locks: stock.1.mutation_locks.load(Ordering::Relaxed),
            membership_attempts: stock.1.caster_memberships.load(Ordering::Relaxed),
            retained_views: state.views.len(),
            retained_memberships: state.views.values().map(|view| view.casters.len()).sum(),
            point_locks,
            point_membership_attempts,
        }
    }
    // Run the actual pre-fix helper workload, then the same workload admitted
    // through the production directional-only gate. No GPU or wall-time bar.
    let before = measure(false, &directional);
    let after = measure(true, &directional);
    assert_eq!(
        before.point_locks,
        u64::from(POINT_VIEWS) * (2 + 2 * u64::from(CASTERS))
    );
    assert_eq!(
        before.point_membership_attempts,
        u64::from(POINT_VIEWS) * 2 * u64::from(CASTERS)
    );
    assert_eq!((after.point_locks, after.point_membership_attempts), (0, 0));
    assert_eq!(
        before.retained_views,
        (POINT_VIEWS + DIRECTIONAL_VIEWS) as usize
    );
    assert_eq!(
        before.retained_memberships,
        ((POINT_VIEWS + DIRECTIONAL_VIEWS) * CASTERS) as usize
    );
    assert_eq!(after.retained_views, DIRECTIONAL_VIEWS as usize);
    assert_eq!(
        after.retained_memberships,
        (DIRECTIONAL_VIEWS * CASTERS) as usize
    );
    assert_eq!(
        after.locks,
        u64::from(DIRECTIONAL_VIEWS) * (2 + 2 * u64::from(CASTERS))
    );
    assert_eq!(
        after.membership_attempts,
        2 * u64::from(DIRECTIONAL_VIEWS * CASTERS)
    );
    assert_eq!(before.locks - after.locks, before.point_locks);
    assert_eq!(
        before.membership_attempts - after.membership_attempts,
        before.point_membership_attempts
    );
}

fn late_pass_world(frame: u32) -> World {
    let mut world = World::new();
    world.insert_resource(FrameCount(frame));
    world.insert_resource(DrawFunctions::<Shadow>::default());
    world.insert_resource(ShadowMapExternalInputs::default());
    world
}

#[test]
fn poisoned_cache_keeps_real_early_and_late_occlusion_rendering() {
    let cache = cache();
    rendered(&cache, view(2), key());
    poison(&cache.0);
    for frame in 2..=4 {
        let world = late_pass_world(frame);
        assert!(!cache.begin(&world, entity(5), view(2), false, "cascade"));
        cache.finish(view(2), false, true, true);
        assert!(!cache.begin(&world, entity(5), view(2), true, "cascade"));
        cache.finish(view(2), true, true, true);
        assert!(cache.maps().owners.is_empty());
        assert!(cache.maps().retention_disabled);
    }
}

#[test]
fn poison_after_early_reuse_skips_only_that_frames_late_append() {
    let cache = cache();
    let key = key();
    rendered(&cache, view(2), key.clone());
    assert!(begin(&cache, view(2), 2, key));
    poison(&cache.0);
    let world = late_pass_world(2);
    assert!(cache.begin(&world, entity(5), view(2), true, "cascade"));
    let world = late_pass_world(3);
    assert!(!cache.begin(&world, entity(5), view(2), false, "cascade"));
    cache.finish(view(2), false, true, true);
    assert!(!cache.begin(&world, entity(5), view(2), true, "cascade"));
    cache.finish(view(2), true, true, true);
    assert!(cache.maps().owners.is_empty());
}

#[test]
fn replacing_render_assets_invalidates_a_retained_map() {
    let cache = cache();
    let view = view(2);
    let mut old_key = key();
    old_key.meshes = RenderAssets::<RenderMesh>::default().revision();
    rendered(&cache, view, old_key.clone());
    assert!(begin(&cache, view, 2, old_key.clone()));

    let mut new_key = old_key;
    new_key.meshes = RenderAssets::<RenderMesh>::default().revision();
    assert!(!begin(&cache, view, 3, new_key));
}
