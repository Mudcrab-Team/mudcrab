use super::{
    admit_directional_shadow_cascades, directional_shadow_atlas_layout,
    directional_shadow_atlas_usage, reserve_directional_shadow_slices,
    spot_shadow_atlas_needs_update, spot_shadow_copy_region, RetainedDirectionalShadowTexture,
};
use bevy_ecs::{entity::EntityIndex, prelude::Entity};
use bevy_render::render_resource::{Extent3d, Origin3d, TextureId, TextureUsages};

fn camera(index: u32) -> Entity {
    Entity::from_index(EntityIndex::from_raw_u32(index).unwrap())
}

#[test]
fn two_active_cameras_reserve_the_sum_and_distinct_cascade_slices() {
    let mut total = 0;
    let main = reserve_directional_shadow_slices(&mut total, 4);
    let secondary = reserve_directional_shadow_slices(&mut total, 4);
    assert_eq!(total, 8);
    assert_eq!(main, 0..4);
    assert_eq!(secondary, 4..8);
    assert!(main.clone().all(|slice| !secondary.contains(&slice)));
    // GPU light bases and depth attachments use the same camera range start.
    let main_light_bases = [main.start, main.start + 2];
    let secondary_light_bases = [secondary.start, secondary.start + 2];
    assert_eq!(main_light_bases, [0, 2]);
    assert_eq!(secondary_light_bases, [4, 6]);
}

#[test]
fn empty_camera_uses_no_slices_and_spots_follow_all_cameras() {
    let mut total = 0;
    assert_eq!(reserve_directional_shadow_slices(&mut total, 3), 0..3);
    assert_eq!(reserve_directional_shadow_slices(&mut total, 0), 3..3);
    assert_eq!(reserve_directional_shadow_slices(&mut total, 2), 3..5);
    let first_spot = total;
    assert_eq!(first_spot, 5);
}

#[test]
fn original_256_layer_budget_keeps_two_four_cascade_cameras_and_all_252_spots() {
    let main = camera(1);
    let secondary = camera(2);
    let layout = directional_shadow_atlas_layout(vec![(main, 4), (secondary, 4)], 252, 256);
    assert!(layout.overflow);
    assert_eq!(layout.layers, 256);
    assert_eq!(layout.camera_slices, [(main, 0..4), (secondary, 0..4)]);
    assert_eq!(layout.spot_start, 4);
    assert_eq!(layout.camera_atlas(main), None);
    assert_eq!(layout.camera_atlas(secondary), Some(secondary));
    assert_eq!(layout.extra_cameras().collect::<Vec<_>>(), [secondary]);

    // The master and the secondary retain different physical directional maps,
    // while every spot copies between those textures at the same shader index.
    let master_texture = TextureId::new();
    let secondary_texture = TextureId::new();
    assert_ne!(master_texture, secondary_texture);
    let copy_destinations: Vec<_> = layout
        .extra_cameras()
        .map(|owner| {
            assert_eq!(owner, secondary);
            secondary_texture
        })
        .collect();
    assert_eq!(copy_destinations, [secondary_texture]);
    for spot in 0..252 {
        let shader_layer = (layout.spot_start + spot) as u32;
        let (origin, size) = spot_shadow_copy_region(
            Extent3d {
                width: 2048,
                height: 2048,
                depth_or_array_layers: layout.layers as u32,
            },
            shader_layer,
        );
        assert_eq!(
            origin,
            Origin3d {
                x: 0,
                y: 0,
                z: shader_layer
            }
        );
        assert_eq!(
            size,
            Extent3d {
                width: 2048,
                height: 2048,
                depth_or_array_layers: 1
            }
        );
        assert!((4..256).contains(&origin.z));
    }
}

#[test]
fn exact_256_layer_sum_keeps_disjoint_fast_path_without_copy_usages() {
    let main = camera(1);
    let secondary = camera(2);
    let layout = directional_shadow_atlas_layout(vec![(main, 4), (secondary, 4)], 248, 256);
    assert!(!layout.overflow);
    assert_eq!(layout.layers, 256);
    assert_eq!(layout.camera_slices, [(main, 0..4), (secondary, 4..8)]);
    assert_eq!(layout.spot_start, 8);
    assert_eq!(layout.camera_atlas(main), None);
    assert_eq!(layout.camera_atlas(secondary), None);
    assert_eq!(layout.extra_cameras().count(), 0);
    assert_eq!(
        directional_shadow_atlas_usage(false),
        TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING
    );
    assert!(directional_shadow_atlas_usage(true)
        .contains(TextureUsages::COPY_SRC | TextureUsages::COPY_DST));
}

#[test]
fn differing_layer_filtered_cameras_share_spot_indices_above_largest_local_cascade_range() {
    let main = camera(1);
    let empty = camera(2);
    let secondary = camera(3);
    let layout =
        directional_shadow_atlas_layout(vec![(main, 4), (empty, 0), (secondary, 2)], 252, 256);
    assert!(layout.overflow);
    assert_eq!(layout.layers, 256);
    assert_eq!(
        layout.camera_slices,
        [(main, 0..4), (empty, 0..0), (secondary, 0..2)]
    );
    assert_eq!(layout.spot_start, 4);
    assert_eq!(
        layout.extra_cameras().collect::<Vec<_>>(),
        [empty, secondary]
    );
    // Even the camera without directional lights needs the complete spot atlas.
    assert_eq!(layout.camera_atlas(empty), Some(empty));
    assert_eq!(layout.camera_atlas(secondary), Some(secondary));
    for (_, cascades) in &layout.camera_slices {
        assert!(cascades.end <= layout.spot_start);
    }
}

#[test]
fn differing_cameras_without_spots_keep_compact_disjoint_ranges() {
    let main = camera(1);
    let secondary = camera(2);
    let empty = camera(3);
    let layout =
        directional_shadow_atlas_layout(vec![(main, 4), (secondary, 2), (empty, 0)], 0, 256);
    assert!(!layout.overflow);
    assert_eq!(layout.layers, 6);
    assert_eq!(
        layout.camera_slices,
        [(main, 0..4), (secondary, 4..6), (empty, 6..6)]
    );
    assert_eq!(layout.extra_cameras().count(), 0);
}

#[test]
fn many_cameras_without_spots_overflow_into_independent_small_atlases() {
    let layout = directional_shadow_atlas_layout(
        (1..=70)
            .map(|index| (camera(index), if index == 70 { 2 } else { 4 }))
            .collect(),
        0,
        256,
    );
    assert!(layout.overflow);
    assert_eq!(layout.layers, 4);
    assert_eq!(layout.spot_start, 4);
    assert_eq!(layout.extra_cameras().count(), 69);
    assert_eq!(layout.camera_slices[0], (camera(1), 0..4));
    assert_eq!(layout.camera_slices[69], (camera(70), 0..2));
}

#[test]
fn camera_removal_returns_to_fast_path_and_invalidates_previous_physical_owners() {
    let main = camera(1);
    let secondary = camera(2);
    let before = directional_shadow_atlas_layout(vec![(main, 4), (secondary, 4)], 252, 256);
    let retained = RetainedDirectionalShadowTexture {
        camera_slices: before.camera_slices.clone(),
        spot_start: before.spot_start,
        overflow: before.overflow,
        ..Default::default()
    };
    assert!(!before.topology_changed(&retained));
    let after = directional_shadow_atlas_layout(vec![(main, 4)], 252, 256);
    assert!(after.topology_changed(&retained));
    assert!(!after.overflow);
    assert_eq!(after.layers, 256);
    // No extra keys survive the production retain predicate on this transition.
    assert_eq!(after.extra_cameras().count(), 0);
    assert_eq!(after.camera_slices, [(main, 0..4)]);
    let source = TextureId::new();
    assert!(spot_shadow_atlas_needs_update(
        after.topology_changed(&retained),
        Some((source, 4)),
        (source, 4),
    ));
}

#[test]
fn master_removal_reassigns_the_source_and_prunes_its_old_extra_atlas_key() {
    let first = camera(1);
    let second = camera(2);
    let third = camera(3);
    let before =
        directional_shadow_atlas_layout(vec![(first, 4), (second, 4), (third, 4)], 252, 256);
    let retained = RetainedDirectionalShadowTexture {
        camera_slices: before.camera_slices.clone(),
        spot_start: before.spot_start,
        overflow: before.overflow,
        ..Default::default()
    };
    let after = directional_shadow_atlas_layout(vec![(second, 4), (third, 4)], 252, 256);
    assert!(after.topology_changed(&retained));
    assert!(after.overflow);
    assert_eq!(before.extra_cameras().collect::<Vec<_>>(), [second, third]);
    assert_eq!(after.extra_cameras().collect::<Vec<_>>(), [third]);
    assert_eq!(after.camera_atlas(second), None);
    assert_eq!(after.camera_atlas(third), Some(third));
}

#[test]
fn recreated_camera_never_inherits_a_removed_cameras_physical_owner_key() {
    let main = camera(1);
    let removed = camera(2);
    let replacement = camera(3);
    let before = directional_shadow_atlas_layout(vec![(main, 4), (removed, 4)], 252, 256);
    let retained = RetainedDirectionalShadowTexture {
        camera_slices: before.camera_slices,
        spot_start: before.spot_start,
        overflow: before.overflow,
        ..Default::default()
    };
    let after = directional_shadow_atlas_layout(vec![(main, 4), (replacement, 4)], 252, 256);
    assert!(after.topology_changed(&retained));
    assert_eq!(after.extra_cameras().collect::<Vec<_>>(), [replacement]);
    let master = TextureId::new();
    // The source and spot index may be unchanged; destinations still must refresh.
    assert!(spot_shadow_atlas_needs_update(
        true,
        Some((master, 255)),
        (master, 255)
    ));
}

#[test]
fn unchanged_spot_rebuilds_for_new_source_texture_layout_or_destination_set() {
    let original = TextureId::new();
    let recreated = TextureId::new();
    assert!(!spot_shadow_atlas_needs_update(
        false,
        Some((original, 4)),
        (original, 4)
    ));
    assert!(spot_shadow_atlas_needs_update(
        false,
        Some((original, 4)),
        (recreated, 4)
    ));
    assert!(spot_shadow_atlas_needs_update(
        false,
        Some((original, 8)),
        (original, 4)
    ));
    assert!(spot_shadow_atlas_needs_update(
        true,
        Some((original, 4)),
        (original, 4)
    ));
    assert!(spot_shadow_atlas_needs_update(false, None, (original, 4)));
}

#[test]
fn before_camera_prepare_and_uniforms_apply_the_same_per_camera_admission_limit() {
    let mut before_camera_count = 0;
    let prepared: Vec<_> = [4, 4, 2]
        .into_iter()
        .map(|cascades| admit_directional_shadow_cascades(&mut before_camera_count, cascades, 6))
        .collect();
    assert_eq!(prepared, [4, 0, 2]);
    assert_eq!(before_camera_count, 6);
    let mut uniform_count = 0;
    let mut uniform_bases = Vec::new();
    for cascades in [4, 4, 2] {
        let admitted = admit_directional_shadow_cascades(&mut uniform_count, cascades, 6);
        uniform_bases.push(uniform_count - admitted);
    }
    assert_eq!(uniform_bases, [0, 4, 4]);
    assert_eq!(uniform_count, before_camera_count);
    let layout = directional_shadow_atlas_layout(vec![(camera(1), before_camera_count)], 0, 6);
    assert_eq!(layout.camera_slices, [(camera(1), 0..6)]);
}

#[test]
fn budget_reduction_despawns_excluded_spot_views_and_releases_atlas_handles() {
    use super::{discard_unadmitted_spot_views, PointAndSpotLightViewEntities};
    use alloc::sync::Arc;
    use bevy_ecs::{
        component::Component,
        system::Commands,
        world::{CommandQueue, World},
    };
    #[derive(Component)]
    struct AtlasHandle(Arc<()>);

    let mut world = World::new();
    let atlas = Arc::new(());
    let weak = Arc::downgrade(&atlas);
    let mut views: Vec<_> = (0..252)
        .map(|_| PointAndSpotLightViewEntities(vec![world.spawn(AtlasHandle(atlas.clone())).id()]))
        .collect();
    let mut queue = CommandQueue::default();
    {
        let mut commands = Commands::new(&mut queue, &world);
        for (index, view) in views.iter_mut().enumerate() {
            assert_eq!(
                discard_unadmitted_spot_views(&mut commands, view, index < 248),
                index >= 248
            );
        }
    }
    queue.apply(&mut world);
    assert_eq!(Arc::strong_count(&atlas), 249);
    assert!(views[248..].iter().all(|view| view.0.is_empty()));
    assert!(views[..248].iter().all(|view| view.0.len() == 1));
    // Removing the extra camera replaces the remaining admitted components.
    // Excluded views must not retain one last clone of that old allocation.
    for view in &views[..248] {
        let removed = world.entity_mut(view.0[0]).take::<AtlasHandle>().unwrap();
        assert!(Arc::ptr_eq(&removed.0, &atlas));
    }
    drop(atlas);
    assert!(weak.upgrade().is_none());
}

#[test]
fn spot_readmission_restarts_without_a_stale_render_entity() {
    use super::{discard_unadmitted_spot_views, PointAndSpotLightViewEntities};
    use bevy_ecs::{
        system::Commands,
        world::{CommandQueue, World},
    };
    let mut world = World::new();
    let old = world.spawn_empty().id();
    let mut views = PointAndSpotLightViewEntities(vec![old]);
    let mut queue = CommandQueue::default();
    assert!(discard_unadmitted_spot_views(
        &mut Commands::new(&mut queue, &world),
        &mut views,
        false
    ));
    queue.apply(&mut world);
    assert!(world.get_entity(old).is_err());
    assert!(!discard_unadmitted_spot_views(
        &mut Commands::new(&mut queue, &world),
        &mut views,
        true
    ));
    assert!(views.0.is_empty()); // The existing creation branch must allocate a new view.
}
