use super::*;

fn scene() -> ClusterScene {
    ClusterScene::new(
        ClusterMeshAsset::from_bytes(
            include_bytes!("../../../shared/tests/data/cluster/fixture.mcl").to_vec(),
        )
        .unwrap(),
        &[],
    )
}

#[test]
fn upload_failure_receipt_waits_for_main_channel_and_is_consumed_once() {
    let failures = UploadFailures::default();
    let scene = scene();
    let mut main = bevy::render::MainWorld::default();
    main.insert_resource(scene.clone());
    let source_tick = main
        .get_resource_ref::<ClusterScene>()
        .unwrap()
        .last_changed();
    failures.report(
        scene.ready_revision.clone(),
        source_tick,
        "corrupt rebuild".into(),
    );
    failures.publish(&mut main);
    assert_eq!(failures.0.lock().unwrap().len(), 1);
    assert!(!failures.0.lock().unwrap()[0].delivered);
    main.insert_resource(Messages::<AppExit>::default());
    failures.publish(&mut main);
    failures.report(
        scene.ready_revision,
        source_tick,
        "later error must not overwrite first receipt".into(),
    );
    failures.publish(&mut main);
    assert_eq!(main.resource::<Messages<AppExit>>().len(), 1);
    assert_eq!(
        *main.resource::<super::super::package::PackageValidationStatus>(),
        super::super::package::PackageValidationStatus::Failed("corrupt rebuild".into())
    );
    assert!(failures.0.lock().unwrap()[0].delivered);
}

#[test]
fn late_replaced_scene_failure_cannot_terminate_a_new_publication() {
    let failures = UploadFailures::default();
    let old = scene();
    let replacement = scene();
    let mut main = bevy::render::MainWorld::default();
    main.insert_resource(Messages::<AppExit>::default());
    main.insert_resource(replacement.clone());
    let source_tick = main
        .get_resource_ref::<ClusterScene>()
        .unwrap()
        .last_changed();
    failures.report(
        old.ready_revision.clone(),
        source_tick,
        "stale worker".into(),
    );
    failures.publish(&mut main);
    assert!(main.resource::<Messages<AppExit>>().is_empty());
    assert!(
        main.get_resource::<super::super::package::PackageValidationStatus>()
            .is_none()
    );
    assert!(failures.0.lock().unwrap().is_empty());
    // A late old task may finish after replacement was already extracted, too.
    failures.report(
        old.ready_revision,
        source_tick,
        "even later stale worker".into(),
    );
    failures.publish(&mut main);
    assert!(main.resource::<Messages<AppExit>>().is_empty());
    failures.report(
        replacement.ready_revision,
        source_tick,
        "current worker failed".into(),
    );
    failures.publish(&mut main);
    assert_eq!(main.resource::<Messages<AppExit>>().len(), 1);
}

#[test]
fn upload_estimate_counts_shared_payload_and_words_once() {
    let mut scene = scene();
    scene.meshes[0].prepare_tangents().unwrap();
    let one = scene.mesh_resident_estimate().unwrap();
    scene.meshes.push(scene.meshes[0].clone());
    let two = scene.mesh_resident_estimate().unwrap();
    assert_eq!(two.raw_payload_bytes, one.raw_payload_bytes);
    assert_eq!(two.tangent_bytes, one.tangent_bytes);
    assert_eq!(two.rehydrating_meshes, one.rehydrating_meshes);
}

#[test]
fn post_upload_pool_sizes_and_mesh_info_need_only_retained_metadata() {
    let mesh = ClusterMeshAsset::from_shared_bytes(
        include_bytes!("../../../shared/tests/data/cluster/fixture.mcl")
            .to_vec()
            .into(),
    )
    .unwrap();
    mesh.prepare_tangents().unwrap();
    let sizes = mesh_pool_sizes(&mesh);
    let row = mesh_info_row(&PoolCursors::default(), &mesh, false);
    mesh.with_upload(|view, words| {
        let sections = mesh_sections(&mesh, view, words);
        assert_eq!(sizes.positions, sections.positions.len() as u64);
        assert_eq!(sizes.attributes, sections.attributes.len() as u64);
        assert_eq!(sizes.clusters, sections.clusters.len() as u64);
        assert_eq!(
            sizes.cluster_vertices,
            sections.cluster_vertices.len() as u64
        );
        assert_eq!(sizes.cluster_indices, sections.cluster_indices.len() as u64);
        assert_eq!(sizes.groups, sections.groups.len() as u64);
    })
    .unwrap()
    .unwrap();
    assert!(mesh.snapshot().is_err());
    assert_eq!(mesh_pool_sizes(&mesh), sizes);
    assert_eq!(mesh_info_row(&PoolCursors::default(), &mesh, false), row);
    assert_eq!(mesh.resident_estimate().unwrap().raw_payload_bytes, 0);
    assert_eq!(mesh.resident_estimate().unwrap().tangent_bytes, 0);
}

#[test]
fn unchanged_upload_revision_does_no_mesh_or_block_work() {
    let mut uploads = PendingBlockUploads::new(2);
    let mut visits = 0;
    uploads.update(0, |_| {
        visits += 1;
        false
    });
    assert_eq!(visits, 2);
    for _ in 0..128 {
        uploads.update(0, |_| panic!("settled revision rescanned a block or mesh"));
    }
    assert_eq!(uploads.pop(), None);
    uploads.update(1, |_| true);
    assert_eq!(uploads.pop(), Some(0));
    assert_eq!(uploads.pop(), Some(1));
    for revision in 2..130 {
        uploads.update(revision, |_| panic!("uploaded blocks were revisited"));
    }
}

#[test]
fn removed_scene_discards_its_old_error_receipt() {
    let failures = UploadFailures::default();
    failures.report(
        scene().ready_revision,
        bevy::ecs::change_detection::Tick::new(1),
        "removed scene".into(),
    );
    let mut main = bevy::render::MainWorld::default();
    main.insert_resource(Messages::<AppExit>::default());
    failures.publish(&mut main);
    assert!(failures.0.lock().unwrap().is_empty());
    assert!(main.resource::<Messages<AppExit>>().is_empty());
}

#[test]
fn cloned_structural_replacement_discards_old_receipt_and_accepts_its_own_failure() {
    let failures = UploadFailures::default();
    let old = scene();
    let mut main = bevy::render::MainWorld::default();
    main.insert_resource(Messages::<AppExit>::default());
    main.insert_resource(old.clone());
    let old_tick = main
        .get_resource_ref::<ClusterScene>()
        .unwrap()
        .last_changed();
    failures.report(
        old.ready_revision.clone(),
        old_tick,
        "old upload failed".into(),
    );
    let mut replacement = old.clone();
    replacement.meshes.clear();
    replacement.instances.clear();
    replacement.blocks.clear();
    main.increment_change_tick();
    main.insert_resource(replacement.clone());
    failures.publish(&mut main);
    assert!(
        main.resource::<Messages<AppExit>>().is_empty(),
        "stale cloned-publication error escaped"
    );
    assert!(
        main.get_resource::<super::super::package::PackageValidationStatus>()
            .is_none()
    );
    let new_tick = main
        .get_resource_ref::<ClusterScene>()
        .unwrap()
        .last_changed();
    assert_ne!(old_tick, new_tick);
    failures.report(
        replacement.ready_revision,
        new_tick,
        "new publication failed".into(),
    );
    failures.publish(&mut main);
    assert_eq!(main.resource::<Messages<AppExit>>().len(), 1);
    assert_eq!(
        main.resource::<super::super::package::PackageValidationStatus>(),
        &super::super::package::PackageValidationStatus::Failed("new publication failed".into())
    );
}

fn assert_preparation_failure_is_terminal(
    scene: ClusterScene,
    result: Result<(), String>,
    expected: &str,
) {
    let reason = result.as_ref().unwrap_err().clone();
    assert!(
        reason.contains(expected),
        "unexpected validation failure: {reason}"
    );
    let failures = UploadFailures::default();
    let mut main = bevy::render::MainWorld::default();
    main.insert_resource(Messages::<AppExit>::default());
    main.insert_resource(scene.clone());
    let source_tick = main
        .get_resource_ref::<ClusterScene>()
        .unwrap()
        .last_changed();
    assert!(preparation_result(result, &scene, source_tick, &failures).is_none());
    failures.publish(&mut main);
    failures.publish(&mut main);
    let exits = main.resource::<Messages<AppExit>>();
    assert_eq!(
        exits.len(),
        1,
        "GPU preparation refusal must terminate instead of leaving loading pending"
    );
    assert!(matches!(
        exits.iter_current_update_messages().next(),
        Some(AppExit::Error(_))
    ));
    assert_eq!(
        main.resource::<super::super::package::PackageValidationStatus>(),
        &super::super::package::PackageValidationStatus::Failed(reason)
    );
}

#[test]
fn unsupported_gpu_preparation_surfaces_one_terminal_failure() {
    assert_preparation_failure_is_terminal(
        scene(),
        super::super::gpu_support(
            bevy::render::settings::WgpuFeatures::empty(),
            &bevy::render::settings::WgpuLimits::default(),
        ),
        "INDIRECT_FIRST_INSTANCE",
    );
}

#[test]
fn oversized_gpu_preparation_surfaces_one_terminal_failure() {
    let scene = scene();
    let totals = PoolCursors {
        positions: u64::MAX,
        ..Default::default()
    };
    let result = check_scene_limits(
        &bevy::render::settings::WgpuLimits::default(),
        &totals,
        &scene,
    );
    assert_preparation_failure_is_terminal(scene, result, "positions");
}

#[test]
fn mixed_tangent_gpu_preparation_surfaces_one_terminal_failure() {
    let mut scene = scene();
    let mut second_mesh = scene.meshes[0].clone();
    second_mesh.flags ^= shared::cluster::FLAG_TANGENTS;
    scene.meshes.push(second_mesh);
    for mesh in 0..2 {
        scene.instances.push(ClusterInstance {
            identity: ClusterInstanceIdentity::Fixture,
            mesh,
            batch: 0,
            world_from_local: Mat4::IDENTITY,
            source_worldspace: 0,
        });
    }
    let result = batch_source_tangents(&scene, 1)
        .map(|_| ())
        .map_err(String::from);
    assert_preparation_failure_is_terminal(scene, result, "mixes authored tangent layouts");
}
