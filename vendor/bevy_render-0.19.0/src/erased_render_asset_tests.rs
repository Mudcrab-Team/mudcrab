use super::*;
use bevy_ecs::{system::RunSystemOnce, world::World};
use bevy_shader::Shader;

#[test]
fn prepared_asset_deltas_are_exact_and_drained_without_a_population_walk() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let used = AssetId::<Shader>::default();
    let unrelated = AssetId::<Shader>::invalid();
    assets.insert(used, 1);
    let first_revision = assets.asset_revision(used);
    assert_eq!(assets.take_changed_assets(), vec![used.untyped()]);
    assets.insert(unrelated, 2);
    assert_eq!(assets.asset_revision(used), first_revision);
    assert_eq!(assets.take_changed_assets(), vec![unrelated.untyped()]);
    assert!(assets.take_changed_assets().is_empty());
    assets.get(used);
    assert!(assets.take_changed_assets().is_empty());
    assets.get_mut(used);
    assert_eq!(assets.take_changed_assets(), vec![used.untyped()]);
    assets.remove(used);
    assert_eq!(assets.asset_revision(used), None);
    assert_eq!(assets.take_changed_assets(), vec![used.untyped()]);
}

#[test]
fn erased_assets_revision_tracks_insert_replace_and_remove() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let id = AssetId::<Shader>::default();
    assert_eq!(assets.revision(), 0);
    assert_eq!(assets.insert(id, 1), None);
    assert_eq!(assets.revision(), 1);
    assert_eq!(assets.insert(id, 2), Some(1));
    assert_eq!(assets.revision(), 2);
    assert_eq!(assets.remove(id), Some(2));
    assert_eq!(assets.revision(), 3);
    assert_eq!(assets.remove(id), None);
    assert_eq!(assets.revision(), 3);
}

#[test]
fn erased_assets_revision_tracks_mutable_access_but_not_reads_or_misses() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let id = AssetId::<Shader>::default();
    assets.insert(id, 1);
    assert_eq!(assets.get(id), Some(&1));
    assert_eq!(assets.iter().count(), 1);
    assert_eq!(assets.get_mut(AssetId::<Shader>::invalid()), None);
    assert_eq!(assets.revision(), 1);
    *assets.get_mut(id).unwrap() = 2;
    assert_eq!(assets.revision(), 2);
    for (_, value) in assets.iter_mut() {
        *value += 1;
    }
    assert_eq!(assets.get(id), Some(&3));
    assert_eq!(assets.revision(), 3);
}

#[test]
fn erased_assets_read_changes_requires_a_baseline_for_each_new_reader() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));

    let id = AssetId::<Shader>::default();
    assets.insert(id, 1);
    let mut late_reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut late_reader), None);
    assert_eq!(assets.read_changes(&mut late_reader), Some(vec![]));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![id.untyped()]));
}

#[test]
fn erased_assets_read_changes_expired_cursor_requires_a_new_baseline() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let id = AssetId::<Shader>::default();
    let mut fast = ErasedRenderAssetChangeReader::default();
    let mut slow = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut fast), None);
    assert_eq!(assets.read_changes(&mut slow), None);
    assets.insert(id, 1);
    assert_eq!(assets.read_changes(&mut fast), Some(vec![id.untyped()]));
    assets.4.update();
    assets.4.update();
    assert_eq!(assets.read_changes(&mut fast), Some(vec![]));
    assert_eq!(assets.read_changes(&mut slow), None);
    assert_eq!(assets.read_changes(&mut slow), Some(vec![]));
    assets.remove(id);
    assert_eq!(assets.read_changes(&mut slow), Some(vec![id.untyped()]));
}

#[test]
fn erased_assets_read_changes_tracks_insert_replace_mutable_access_and_remove() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let id = AssetId::<Shader>::default();
    let unrelated = AssetId::<Shader>::invalid();
    assets.insert(unrelated, 10);
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);

    assert_eq!(assets.insert(id, 1), None);
    assert_eq!(assets.read_changes(&mut reader), Some(vec![id.untyped()]));
    assert_eq!(assets.insert(id, 2), Some(1));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![id.untyped()]));
    *assets.get_mut(id).unwrap() = 3;
    assert_eq!(assets.read_changes(&mut reader), Some(vec![id.untyped()]));
    assert_eq!(assets.remove(id), Some(3));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![id.untyped()]));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));
    assert_eq!(assets.get(unrelated), Some(&10));
}

#[test]
fn erased_assets_read_changes_tracks_every_asset_exposed_by_iter_mut() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let shader = AssetId::<Shader>::default();
    let image = AssetId::<bevy_image::Image>::default();
    assets.insert(shader, 1);
    assets.insert(image, 2);
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);

    for (_, value) in assets.iter_mut() {
        *value += 10;
    }

    let changed = assets.read_changes(&mut reader).unwrap();
    assert_eq!(changed.len(), 2);
    assert!(changed.contains(&shader.untyped()));
    assert!(changed.contains(&image.untyped()));
    assert_eq!(assets.get(shader), Some(&11));
    assert_eq!(assets.get(image), Some(&12));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));
}

#[test]
fn erased_assets_read_changes_ignores_reads_and_missing_mutations() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let id = AssetId::<Shader>::default();
    let missing = AssetId::<Shader>::invalid();
    assets.insert(id, 1);
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);

    assert_eq!(assets.get(id), Some(&1));
    assert_eq!(assets.iter().count(), 1);
    assert_eq!(assets.get_mut(missing), None);
    assert_eq!(assets.remove(missing), None);
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));
}

#[test]
fn erased_assets_read_changes_ignores_iter_mut_on_an_empty_collection() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);

    assert_eq!(assets.iter_mut().count(), 0);

    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));
}

#[test]
fn erased_assets_read_changes_accumulates_and_deduplicates_between_reads() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let used = AssetId::<Shader>::default();
    let other_changed = AssetId::<Shader>::invalid();
    let unchanged = AssetId::<bevy_image::Image>::default();
    assets.insert(used, 1);
    assets.insert(unchanged, 10);
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);

    assets.insert(used, 2);
    *assets.get_mut(used).unwrap() = 3;
    assets.insert(other_changed, 4);
    assert_eq!(assets.remove(used), Some(3));
    assets.insert(used, 5);
    assert_eq!(assets.remove(other_changed), Some(4));

    let changed = assets.read_changes(&mut reader).unwrap();
    assert_eq!(changed.len(), 2);
    assert!(changed.contains(&used.untyped()));
    assert!(changed.contains(&other_changed.untyped()));
    assert!(!changed.contains(&unchanged.untyped()));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));
}

#[test]
fn erased_assets_read_changes_keeps_independent_reader_windows() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let used = AssetId::<Shader>::default();
    let unrelated = AssetId::<Shader>::invalid();
    assets.insert(used, 1);
    assets.insert(unrelated, 2);
    let mut fast_reader = ErasedRenderAssetChangeReader::default();
    let mut slow_reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut fast_reader), None);
    assert_eq!(assets.read_changes(&mut slow_reader), None);

    assets.insert(used, 3);
    assert_eq!(
        assets.read_changes(&mut fast_reader),
        Some(vec![used.untyped()])
    );
    assert_eq!(assets.read_changes(&mut fast_reader), Some(vec![]));

    assets.insert(unrelated, 4);
    assert_eq!(
        assets.read_changes(&mut fast_reader),
        Some(vec![unrelated.untyped()])
    );
    let slow_changes = assets.read_changes(&mut slow_reader).unwrap();
    assert_eq!(slow_changes.len(), 2);
    assert!(slow_changes.contains(&used.untyped()));
    assert!(slow_changes.contains(&unrelated.untyped()));
    assert_eq!(assets.read_changes(&mut slow_reader), Some(vec![]));

    assets.remove(used);
    assert_eq!(
        assets.read_changes(&mut slow_reader),
        Some(vec![used.untyped()])
    );
    assert_eq!(
        assets.read_changes(&mut fast_reader),
        Some(vec![used.untyped()])
    );
}

#[test]
fn erased_assets_read_changes_is_independent_of_take_changed_assets() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let id = AssetId::<Shader>::default();
    assets.insert(id, 1);
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);
    assert_eq!(assets.take_changed_assets(), vec![id.untyped()]);

    assets.insert(id, 2);
    assert_eq!(assets.take_changed_assets(), vec![id.untyped()]);
    assert!(assets.take_changed_assets().is_empty());
    assert_eq!(assets.read_changes(&mut reader), Some(vec![id.untyped()]));

    assets.remove(id);
    assert_eq!(assets.read_changes(&mut reader), Some(vec![id.untyped()]));
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));
    assert_eq!(assets.take_changed_assets(), vec![id.untyped()]);
    assert!(assets.take_changed_assets().is_empty());
}

#[test]
fn erased_assets_read_changes_survives_dropping_a_reader_with_pending_changes() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let id = AssetId::<Shader>::default();
    assets.insert(id, 1);
    let mut surviving_reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut surviving_reader), None);
    {
        let mut dropped_reader = ErasedRenderAssetChangeReader::default();
        assert_eq!(assets.read_changes(&mut dropped_reader), None);
        assets.insert(id, 2);
    }

    assets.remove(id);
    assets.insert(id, 3);
    assert_eq!(
        assets.read_changes(&mut surviving_reader),
        Some(vec![id.untyped()])
    );
    let mut new_reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut new_reader), None);
    assets.insert(id, 4);
    assert_eq!(
        assets.read_changes(&mut new_reader),
        Some(vec![id.untyped()])
    );
    assert_eq!(
        assets.read_changes(&mut surviving_reader),
        Some(vec![id.untyped()])
    );
}

#[test]
fn erased_assets_read_changes_resets_when_replaced_at_the_same_revision() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let old_id = AssetId::<Shader>::default();
    let new_id = AssetId::<Shader>::invalid();
    let unrelated = AssetId::<bevy_image::Image>::default();
    assets.insert(old_id, 1);
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);
    assets.insert(old_id, 2);
    assert_eq!(
        assets.read_changes(&mut reader),
        Some(vec![old_id.untyped()])
    );
    let observed_revision = assets.revision();
    assets.remove(old_id);

    assets = ErasedRenderAssets::<u32>::default();
    assets.insert(new_id, 3);
    assets.insert(unrelated, 4);
    assert_eq!(assets.revision(), observed_revision);
    assert_eq!(assets.read_changes(&mut reader), None);
    assert_eq!(assets.read_changes(&mut reader), Some(vec![]));

    assets.insert(new_id, 5);
    assert_eq!(
        assets.read_changes(&mut reader),
        Some(vec![new_id.untyped()])
    );
}

struct RecoveryAsset;

struct SecondRecoveryAsset;

impl ErasedRenderAsset for SecondRecoveryAsset {
    type SourceAsset = Shader;
    type ErasedAsset = u32;
    type Param = ();

    fn prepare_asset(
        _: Shader,
        _: AssetId<Shader>,
        _: &mut SystemParamItem<Self::Param>,
    ) -> Result<u32, PrepareAssetError<Shader>> {
        Ok(2)
    }
}

#[test]
fn erased_assets_shared_collection_rotates_once_per_render_even_when_preinitialized() {
    use bevy_ecs::schedule::Schedule;
    let mut app = App::new();
    let mut render = SubApp::new();
    render.add_schedule(Schedule::new(Render));
    render.configure_sets(
        Render,
        (RenderSystems::ExtractCommands, RenderSystems::PrepareAssets).chain(),
    );
    render.init_resource::<ErasedRenderAssets<u32>>();
    render.init_resource::<RenderAssetBytesPerFrameLimiter>();
    app.insert_sub_app(RenderApp, render);
    ErasedRenderAssetPlugin::<RecoveryAsset>::default().finish(&mut app);
    ErasedRenderAssetPlugin::<SecondRecoveryAsset>::default().finish(&mut app);
    let render = app.sub_app_mut(RenderApp);
    let id = AssetId::<Shader>::default();
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(
        render
            .world()
            .resource::<ErasedRenderAssets<u32>>()
            .read_changes(&mut reader),
        None
    );
    render
        .world_mut()
        .resource_mut::<ErasedRenderAssets<u32>>()
        .insert(id, 1);
    render.world_mut().run_schedule(Render);
    // Two rotations would expire this unread event in its first render cycle.
    assert_eq!(
        render
            .world()
            .resource::<ErasedRenderAssets<u32>>()
            .read_changes(&mut reader),
        Some(vec![id.untyped()])
    );
    render.world_mut().run_schedule(Render);
    assert_eq!(
        render
            .world()
            .resource::<ErasedRenderAssets<u32>>()
            .read_changes(&mut reader),
        Some(vec![])
    );
}

impl ErasedRenderAsset for RecoveryAsset {
    type SourceAsset = Shader;
    type ErasedAsset = u32;
    type Param = ();

    fn prepare_asset(
        _: Shader,
        _: AssetId<Shader>,
        _: &mut SystemParamItem<Self::Param>,
    ) -> Result<u32, PrepareAssetError<Shader>> {
        Ok(1)
    }
}

#[test]
fn erased_assets_revision_tracks_device_recovery_removals_only() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    assets.insert(AssetId::<Shader>::default(), 1);
    assets.insert(AssetId::<bevy_image::Image>::default(), 2);
    let mut world = World::new();
    world.insert_resource(assets);
    world.init_resource::<PrepareNextFrameAssets<RecoveryAsset>>();
    world
        .run_system_once(collect_erased_render_assets_to_reextract::<RecoveryAsset>)
        .unwrap();
    let assets = world.resource::<ErasedRenderAssets<u32>>();
    assert_eq!(assets.revision(), 3);
    assert_eq!(assets.get(AssetId::<Shader>::default()), None);
    assert_eq!(
        assets.get(AssetId::<bevy_image::Image>::default()),
        Some(&2)
    );
    world
        .run_system_once(collect_erased_render_assets_to_reextract::<RecoveryAsset>)
        .unwrap();
    assert_eq!(world.resource::<ErasedRenderAssets<u32>>().revision(), 3);
}

#[test]
fn erased_assets_read_changes_tracks_device_recovery_removals_only() {
    let mut assets = ErasedRenderAssets::<u32>::default();
    let shader = AssetId::<Shader>::default();
    let image = AssetId::<bevy_image::Image>::default();
    assets.insert(shader, 1);
    assets.insert(image, 2);
    let mut reader = ErasedRenderAssetChangeReader::default();
    assert_eq!(assets.read_changes(&mut reader), None);
    let mut world = World::new();
    world.insert_resource(assets);
    world.init_resource::<PrepareNextFrameAssets<RecoveryAsset>>();

    world
        .run_system_once(collect_erased_render_assets_to_reextract::<RecoveryAsset>)
        .unwrap();
    assert_eq!(
        world
            .resource_mut::<ErasedRenderAssets<u32>>()
            .read_changes(&mut reader),
        Some(vec![shader.untyped()])
    );
    assert_eq!(
        world.resource::<ErasedRenderAssets<u32>>().get(image),
        Some(&2)
    );

    world
        .run_system_once(collect_erased_render_assets_to_reextract::<RecoveryAsset>)
        .unwrap();
    assert_eq!(
        world
            .resource_mut::<ErasedRenderAssets<u32>>()
            .read_changes(&mut reader),
        Some(vec![])
    );
}
