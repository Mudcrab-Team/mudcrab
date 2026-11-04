use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Fixture {
    mesh: ClusterMeshAsset,
    package: Arc<crate::cluster::reader::CertifiedPackage>,
    relative: String,
    builder: [u8; 32],
    expected: MeshExpectation,
    // Drop the retained source locks before tempfile cleanup on Windows.
    _directory: tempfile::TempDir,
}

/// A mesh loaded as the package loads it, still holding the bytes it was validated from.
fn fixture() -> Fixture {
    fixture_with(false, false)
}

/// A mesh as the scene keeps it: validated header, a retained source and no bytes.
fn released_fixture() -> Fixture {
    fixture_with(true, false)
}

/// As the package preload keeps it: released, with the tangent proof recorded from the bytes in hand.
fn proved_released_fixture() -> Fixture {
    fixture_with(true, true)
}

fn fixture_with(release: bool, prove: bool) -> Fixture {
    let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
    let package =
        Arc::new(crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap());
    let connection = package.envelope().connection().unwrap();
    let (relative, hash, primitive, content, size, vertices, clusters): (String, String, u32, String, u64, u32, u32) = connection.query_row(
        "SELECT payload_path, mesh_hash, primitive_index, content_hash, payload_bytes, vertex_count, cluster_count FROM cluster_meshes ORDER BY payload_path LIMIT 1",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).unwrap();
    drop(connection);
    let expected = MeshExpectation {
        content_hash: shared::cluster::parse_hash(&content).unwrap(),
        payload_bytes: size,
        mesh_hash: shared::cluster::parse_hash(&hash).unwrap(),
        primitive_index: primitive,
        vertex_count: vertices,
        cluster_count: clusters,
    };
    let builder = shared::cluster::parse_hash(&package.envelope().builder_identity).unwrap();
    let payload = package.envelope().reread_payload(&relative).unwrap();
    let mut mesh = checked_certified_mesh(&payload, builder, &expected).unwrap();
    mesh.set_rehydrate_source(
        MeshRehydrateSource::new(package.clone(), relative.clone(), builder, expected).unwrap(),
    );
    drop(payload);
    if prove {
        mesh.record_tangent_proof().unwrap();
    }
    if release {
        mesh.release_certified_payload();
    }
    Fixture {
        mesh,
        package,
        relative,
        builder,
        expected,
        _directory: directory,
    }
}

fn wait_for_wake(wake: &AtomicU64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while wake.load(Ordering::Acquire) == 0 {
        assert!(
            Instant::now() < deadline,
            "rehydration worker did not publish completion"
        );
        std::thread::yield_now();
    }
}

#[test]
fn unreleased_mesh_uploads_the_bytes_it_was_loaded_with_without_a_reread() {
    let fixture = fixture();
    let original = fixture.mesh.snapshot().unwrap();
    let original_view = shared::cluster::read_mcl_validated(&original).unwrap();
    let mut changed = original.to_vec();
    changed[8] ^= 1;
    std::fs::write(
        fixture.package.envelope().overlay.join(&fixture.relative),
        changed,
    )
    .unwrap();
    fixture.mesh.prepare_tangents().unwrap();
    fixture
        .mesh
        .with_upload(|view, _| {
            assert_eq!(view.builder_identity, original_view.builder_identity);
            for section in [
                shared::cluster::MclSection::Positions,
                shared::cluster::MclSection::Clusters,
            ] {
                assert_eq!(view.section(section), original_view.section(section));
            }
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture.mesh.rebuild_request_count(),
        0,
        "initial upload reread disk"
    );
    assert_eq!(
        fixture.mesh.resident_estimate().unwrap().raw_payload_bytes,
        0
    );
}

#[test]
fn new_allocation_requests_one_verified_reread_and_releases_it_after_upload() {
    bevy::tasks::IoTaskPool::get_or_init(Default::default);
    let fixture = fixture();
    fixture.mesh.prepare_tangents().unwrap();
    let proof = fixture.mesh.prepared_tangents_usable();
    fixture.mesh.with_upload(|_, _| ()).unwrap().unwrap();
    let wake = Arc::new(AtomicU64::new(0));
    let (send, receive) = std::sync::mpsc::channel();
    assert!(
        !fixture
            .mesh
            .request_upload(wake.clone(), move |reason| send.send(reason).unwrap())
            .unwrap()
    );
    for _ in 0..8 {
        fixture
            .mesh
            .request_upload(wake.clone(), |_| panic!("duplicate rebuild failed"))
            .unwrap();
    }
    wait_for_wake(&wake);
    assert!(receive.try_recv().is_err());
    assert_eq!(fixture.mesh.rebuild_request_count(), 1);
    assert_eq!(fixture.mesh.prepared_tangents_usable(), proof);
    assert_eq!(
        fixture.mesh.resident_estimate().unwrap().raw_payload_bytes as u64,
        fixture.expected.payload_bytes
    );
    assert!(
        fixture
            .mesh
            .with_upload(|view, words| {
                assert_eq!(view.mesh_hash, fixture.expected.mesh_hash);
                assert_eq!(words.len(), fixture.expected.vertex_count as usize);
            })
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fixture.mesh.resident_estimate().unwrap().raw_payload_bytes,
        0
    );
    assert_eq!(fixture.mesh.resident_estimate().unwrap().tangent_bytes, 0);
    assert_eq!(
        fixture.mesh.resident_estimate().unwrap().rehydrating_meshes,
        0
    );
}

#[test]
fn corrupt_or_missing_reread_is_terminal_and_never_reuses_old_bytes() {
    bevy::tasks::IoTaskPool::get_or_init(Default::default);
    for missing in [false, true] {
        let fixture = fixture();
        fixture.mesh.prepare_tangents().unwrap();
        let proof = fixture.mesh.prepared_tangents_usable();
        fixture.mesh.with_upload(|_, _| ()).unwrap().unwrap();
        let path = fixture.package.envelope().overlay.join(&fixture.relative);
        if missing {
            std::fs::rename(&path, path.with_extension("held-for-missing-test")).unwrap();
        } else {
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[8] ^= 1; // Same size, structurally legal, different actual SHA256.
            assert!(shared::cluster::read_mcl(&bytes).is_ok());
            std::fs::write(&path, bytes).unwrap();
        }
        let wake = Arc::new(AtomicU64::new(0));
        let (send, receive) = std::sync::mpsc::channel();
        assert!(
            !fixture
                .mesh
                .request_upload(wake.clone(), move |reason| send.send(reason).unwrap())
                .unwrap()
        );
        let reason = receive.recv_timeout(Duration::from_secs(10)).unwrap();
        if !missing {
            assert!(reason.contains("content hash"), "{reason}");
        }
        assert!(reason.contains(&fixture.relative), "{reason}");
        wait_for_wake(&wake);
        assert_eq!(fixture.mesh.prepared_tangents_usable(), proof);
        assert_eq!(
            fixture.mesh.resident_estimate().unwrap().raw_payload_bytes,
            0
        );
        assert_eq!(fixture.mesh.resident_estimate().unwrap().tangent_bytes, 0);
        assert_eq!(
            fixture
                .mesh
                .with_upload(|_, _| panic!("failed rebuild uploaded stale bytes"))
                .unwrap_err(),
            reason
        );
        assert_eq!(
            fixture
                .mesh
                .request_upload(wake, |_| panic!("terminal failure retried"))
                .unwrap_err(),
            reason
        );
        assert_eq!(fixture.mesh.rebuild_request_count(), 1);
    }
}

#[test]
fn reread_reapplies_typed_row_validation_and_retains_the_source_locks() {
    let fixture = fixture();
    let mut wrong = fixture.expected;
    wrong.vertex_count += 1;
    let source = MeshRehydrateSource::new(
        fixture.package.clone(),
        fixture.relative.clone(),
        fixture.builder,
        wrong,
    )
    .unwrap();
    assert!(source.read_mesh().err().unwrap().contains("vertex count"));
    for path in [
        &fixture.package.envelope().root,
        &fixture.package.envelope().overlay,
    ] {
        assert!(
            shared::asset_lock::AssetLock::acquire_exclusive(path)
                .unwrap_err()
                .is_held()
        );
    }
    let digest = fixture.package.envelope().payload_digest(&fixture.relative);
    assert_eq!(digest, Some(fixture.expected.content_hash));
}

#[test]
fn released_mesh_proves_tangents_through_one_reread_and_keeps_no_bytes() {
    let fixture = released_fixture();
    let envelope = fixture.package.envelope();
    let before = envelope.reread_count();
    let estimate = fixture.mesh.resident_estimate().unwrap();
    assert_eq!(estimate.raw_payload_bytes, 0);
    assert_eq!(estimate.tangent_bytes, 0);
    assert_eq!(fixture.mesh.prepared_tangents_usable(), None);
    fixture.mesh.prepare_tangents().unwrap();
    assert!(fixture.mesh.prepared_tangents_usable().is_some());
    assert_eq!(envelope.reread_count(), before + 1);
    let estimate = fixture.mesh.resident_estimate().unwrap();
    assert_eq!(
        estimate.raw_payload_bytes, 0,
        "proof must not keep the bytes"
    );
    assert_eq!(estimate.tangent_bytes, 0);
    // Another block's worker reaching the same mesh finds the proof and reads nothing.
    fixture.mesh.prepare_tangents().unwrap();
    fixture.mesh.prepare_unused_tangent_slots().unwrap();
    assert_eq!(envelope.reread_count(), before + 1);
    assert!(
        fixture.mesh.with_upload(|_, _| ()).unwrap().is_none(),
        "nothing is resident to upload until the upload requests its own reread"
    );
}

#[test]
fn recorded_proof_prepares_a_released_mesh_without_a_read_and_matches_the_read_proof() {
    bevy::tasks::IoTaskPool::get_or_init(Default::default);
    let read = released_fixture();
    read.mesh.prepare_tangents().unwrap();
    let fixture = proved_released_fixture();
    let envelope = fixture.package.envelope();
    let before = envelope.reread_count();
    assert_eq!(fixture.mesh.prepared_tangents_usable(), None);
    fixture.mesh.prepare_tangents().unwrap();
    assert_eq!(
        fixture.mesh.prepared_tangents_usable(),
        read.mesh.prepared_tangents_usable(),
        "the recorded proof is the proof a read would reach"
    );
    assert_eq!(envelope.reread_count(), before, "preparation read the file");
    assert_eq!(
        fixture.mesh.resident_estimate().unwrap().raw_payload_bytes,
        0
    );
    // Upload still re-reads once, regenerates the words and checks the proof it was given.
    let wake = Arc::new(AtomicU64::new(0));
    assert!(
        !fixture
            .mesh
            .request_upload(wake.clone(), |reason| panic!("{reason}"))
            .unwrap()
    );
    wait_for_wake(&wake);
    assert_eq!(envelope.reread_count(), before + 1);
    assert!(fixture.mesh.with_upload(|_, _| ()).unwrap().is_some());
}

#[test]
fn terrain_only_released_mesh_is_prepared_without_reading_its_file() {
    let fixture = released_fixture();
    let before = fixture.package.envelope().reread_count();
    fixture.mesh.prepare_unused_tangent_slots().unwrap();
    assert_eq!(fixture.mesh.prepared_tangents_usable(), Some(false));
    assert_eq!(fixture.package.envelope().reread_count(), before);
}

#[test]
fn released_mesh_is_read_uploaded_and_released_through_rereads_only() {
    bevy::tasks::IoTaskPool::get_or_init(Default::default);
    let fixture = released_fixture();
    let envelope = fixture.package.envelope();
    let before = envelope.reread_count();
    fixture.mesh.prepare_tangents().unwrap();
    let proof = fixture.mesh.prepared_tangents_usable();
    let wake = Arc::new(AtomicU64::new(0));
    let (send, receive) = std::sync::mpsc::channel();
    assert!(
        !fixture
            .mesh
            .request_upload(wake.clone(), move |reason| send.send(reason).unwrap())
            .unwrap()
    );
    wait_for_wake(&wake);
    assert!(receive.try_recv().is_err());
    assert_eq!(
        envelope.reread_count(),
        before + 2,
        "proof read, upload read"
    );
    assert_eq!(fixture.mesh.prepared_tangents_usable(), proof);
    assert_eq!(
        fixture.mesh.resident_estimate().unwrap().raw_payload_bytes as u64,
        fixture.expected.payload_bytes
    );
    assert!(
        fixture
            .mesh
            .with_upload(|view, words| {
                assert_eq!(view.mesh_hash, fixture.expected.mesh_hash);
                assert_eq!(words.len(), fixture.expected.vertex_count as usize);
            })
            .unwrap()
            .is_some()
    );
    let estimate = fixture.mesh.resident_estimate().unwrap();
    assert_eq!(estimate.raw_payload_bytes, 0);
    assert_eq!(estimate.tangent_bytes, 0);
    assert_eq!(envelope.reread_count(), before + 2, "upload adds no read");
}

#[test]
fn released_mesh_refuses_a_file_changed_before_its_tangents_are_proven() {
    for missing in [false, true] {
        let fixture = released_fixture();
        let path = fixture.package.envelope().overlay.join(&fixture.relative);
        if missing {
            std::fs::remove_file(&path).unwrap();
        } else {
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[8] ^= 1; // Same size, structurally legal, different SHA256.
            assert!(shared::cluster::read_mcl(&bytes).is_ok());
            std::fs::write(&path, bytes).unwrap();
        }
        let reason = fixture.mesh.prepare_tangents().unwrap_err();
        assert!(reason.contains(&fixture.relative), "{reason}");
        if !missing {
            assert!(reason.contains("content hash"), "{reason}");
        }
        assert_eq!(fixture.mesh.prepared_tangents_usable(), None);
        assert!(fixture.mesh.with_upload(|_, _| ()).unwrap().is_none());
    }
}

#[test]
fn replacement_scene_can_wait_for_the_same_rebuild_without_duplicate_io() {
    bevy::tasks::IoTaskPool::get_or_init(Default::default);
    let fixture = fixture();
    fixture.mesh.prepare_tangents().unwrap();
    fixture.mesh.with_upload(|_, _| ()).unwrap().unwrap();
    let old = Arc::new(AtomicU64::new(0));
    let replacement = Arc::new(AtomicU64::new(0));
    assert!(
        !fixture
            .mesh
            .request_upload(old.clone(), |reason| panic!("{reason}"))
            .unwrap()
    );
    // Completion may race this request. Both immediate readiness and a registered wake are valid.
    let ready = fixture
        .mesh
        .request_upload(replacement.clone(), |reason| panic!("{reason}"))
        .unwrap();
    wait_for_wake(&old);
    if !ready {
        wait_for_wake(&replacement);
    }
    assert_eq!(fixture.mesh.rebuild_request_count(), 1);
    assert!(fixture.mesh.with_upload(|_, _| ()).unwrap().is_some());
    assert_eq!(
        fixture.mesh.resident_estimate().unwrap().raw_payload_bytes,
        0
    );
    assert_eq!(fixture.mesh.resident_estimate().unwrap().tangent_bytes, 0);
}
