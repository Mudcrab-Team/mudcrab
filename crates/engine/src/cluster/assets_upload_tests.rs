use super::*;

const MCL: &[u8] = include_bytes!("../../../shared/tests/data/cluster/fixture.mcl");

#[test]
fn uploaded_production_payload_has_no_remaining_cpu_owner() {
    let bytes: Arc<[u8]> = generated_tangent_fixture_bytes().into();
    let weak = Arc::downgrade(&bytes);
    let mesh = ClusterMeshAsset::from_shared_bytes(bytes).unwrap();
    let render_clone = mesh.clone();
    let late_worker = mesh.clone();
    assert_eq!(mesh.prepared_tangents_usable(), None);
    mesh.prepare_tangents().unwrap();
    let words = {
        let state = mesh.upload.state.lock().unwrap();
        Arc::downgrade(
            &state
                .payload
                .as_ref()
                .unwrap()
                .tangents
                .as_ref()
                .unwrap()
                .words,
        )
    };
    let before = mesh.resident_estimate().unwrap();
    assert_eq!(before.raw_payload_bytes, MCL.len());
    assert_eq!(before.tangent_bytes, mesh.vertex_count as usize * 4);
    assert!(before.metadata_bytes > 0);
    let lengths = mesh.section_lengths;
    assert_eq!(
        render_clone
            .with_upload(|view, words| {
                assert!(
                    weak.upgrade().is_some(),
                    "bytes must live through queue writes"
                );
                assert_eq!(view.section(MclSection::Positions).len(), lengths[0]);
                assert_eq!(words.len(), mesh.vertex_count as usize);
            })
            .unwrap(),
        Some(())
    );
    // The production handoff above is shared by renderer, main and all worker clones.
    assert!(
        weak.upgrade().is_none(),
        "upload still pins raw MCL across asset clones"
    );
    assert!(
        words.upgrade().is_none(),
        "upload still pins generated tangent words"
    );
    assert_eq!(mesh.prepared_tangents_usable(), Some(true));
    assert!(mesh.tangents_usable());
    std::thread::spawn(move || late_worker.prepare_tangents().unwrap())
        .join()
        .unwrap();
    assert!(
        weak.upgrade().is_none(),
        "late prep regenerated or repinned payload"
    );
    assert!(words.upgrade().is_none());
    assert_eq!(mesh.section_lengths, lengths);
    assert_eq!(mesh.byte_len(), MCL.len());
    let after = mesh.resident_estimate().unwrap();
    assert_eq!(after.raw_payload_bytes, 0);
    assert_eq!(after.tangent_bytes, 0);
    assert_eq!(after.rehydrating_meshes, 0);
    assert_eq!(after.metadata_bytes, before.metadata_bytes);
    assert_eq!(
        render_clone
            .with_upload(|_, _| panic!("released bytes reused"))
            .unwrap(),
        None::<()>
    );
}

#[test]
fn terrain_and_failed_tangent_proof_survive_release_and_late_prep() {
    for terrain_only in [false, true] {
        let mesh = ClusterMeshAsset::from_shared_bytes(MCL.to_vec().into()).unwrap();
        if terrain_only {
            mesh.prepare_unused_tangent_slots().unwrap();
        } else {
            mesh.fail_tangents_for_test();
        }
        let clone = mesh.clone();
        assert_eq!(mesh.prepared_tangents_usable(), Some(false));
        assert!(
            mesh.with_upload(|_, words| assert_eq!(words.len(), mesh.vertex_count as usize))
                .unwrap()
                .is_some()
        );
        clone.prepare_tangents().unwrap();
        assert_eq!(clone.prepared_tangents_usable(), Some(false));
        assert!(!clone.tangents_usable());
        assert_eq!(clone.resident_estimate().unwrap().raw_payload_bytes, 0);
        assert_eq!(clone.resident_estimate().unwrap().tangent_bytes, 0);
    }
}

#[test]
fn authored_proof_and_source_layout_survive_release() {
    let fixture = authored_tangent_fixture();
    let mesh = ClusterMeshAsset::from_shared_bytes(fixture.snapshot().unwrap()).unwrap();
    mesh.prepare_tangents().unwrap();
    let flags = mesh.flags;
    assert_ne!(flags & FLAG_TANGENTS, 0);
    assert!(mesh.with_upload(|_, _| ()).unwrap().is_some());
    assert_eq!(mesh.flags, flags);
    assert_eq!(mesh.prepared_tangents_usable(), Some(true));
    assert_eq!(mesh.resident_estimate().unwrap().tangent_bytes, 0);
    assert!(
        fixture.source_mesh().is_ok(),
        "explicit in-memory owner is independent"
    );
}

#[test]
fn in_memory_fixture_supports_repeat_upload_and_source_inspection() {
    let mesh = ClusterMeshAsset::from_bytes(MCL.to_vec()).unwrap();
    for _ in 0..2 {
        assert!(
            mesh.with_upload(|_, words| assert_eq!(words.len(), mesh.vertex_count as usize))
                .unwrap()
                .is_some()
        );
        assert_eq!(mesh.snapshot().unwrap().as_ref(), MCL);
        assert!(mesh.source_mesh().is_ok());
    }
    assert_eq!(
        mesh.resident_estimate().unwrap().raw_payload_bytes,
        MCL.len()
    );
}

#[test]
fn production_upload_never_generates_pending_tangents_on_the_caller() {
    let mesh = ClusterMeshAsset::from_shared_bytes(MCL.to_vec().into()).unwrap();
    assert_eq!(
        mesh.with_upload(|_, _| panic!("unprepared mesh uploaded"))
            .unwrap(),
        None::<()>
    );
    assert!(!mesh.tangents_usable());
    assert_eq!(mesh.prepared_tangents_usable(), None);
    assert_eq!(mesh.resident_estimate().unwrap().tangent_bytes, 0);
}

#[test]
fn poisoned_upload_lock_is_an_explicit_error() {
    let mesh = ClusterMeshAsset::from_shared_bytes(MCL.to_vec().into()).unwrap();
    let clone = mesh.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = clone.upload.state.lock().unwrap();
            panic!("injected worker panic while owning payload");
        })
        .join()
        .is_err()
    );
    assert!(
        mesh.with_upload(|_, _| ())
            .unwrap_err()
            .contains("poisoned")
    );
    assert!(mesh.prepare_tangents().unwrap_err().contains("poisoned"));
}

#[test]
fn worker_contending_with_handoff_cannot_repin_released_words() {
    let bytes: Arc<[u8]> = generated_tangent_fixture_bytes().into();
    let raw = Arc::downgrade(&bytes);
    let mesh = ClusterMeshAsset::from_shared_bytes(bytes).unwrap();
    mesh.prepare_tangents().unwrap();
    let worker_mesh = mesh.clone();
    let (start, started) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let (attempt, attempting) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.recv().unwrap();
        attempt.send(()).unwrap();
        let result = worker_mesh.prepare_tangents();
        finished.send(result).unwrap();
    });
    mesh.with_upload(|_, _| {
        start.send(()).unwrap();
        attempting
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(raw.upgrade().is_some());
        // The handoff owns the guard until all queue writes have returned.
        assert!(done.try_recv().is_err());
    })
    .unwrap()
    .unwrap();
    done.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert!(raw.upgrade().is_none());
    assert_eq!(mesh.prepared_tangents_usable(), Some(true));
    assert_eq!(mesh.resident_estimate().unwrap().tangent_bytes, 0);
}

#[test]
fn render_requests_do_not_wait_on_tangent_preparation_lock() {
    let mesh = ClusterMeshAsset::from_shared_bytes(MCL.to_vec().into()).unwrap();
    let guard = mesh.upload.state.lock().unwrap();
    let wake = Arc::new(AtomicU64::new(0));
    assert!(
        !mesh
            .request_upload(wake.clone(), |_| panic!("pending preparation failed"))
            .unwrap()
    );
    assert_eq!(
        wake.load(Ordering::Acquire),
        1,
        "contended request must schedule a retry"
    );
    assert_eq!(
        mesh.with_upload(|_, _| panic!("contended upload ran"))
            .unwrap(),
        None::<()>
    );
    drop(guard);
    assert_eq!(mesh.prepared_tangents_usable(), None);
    assert_eq!(mesh.rebuild_request_count(), 0);
}
