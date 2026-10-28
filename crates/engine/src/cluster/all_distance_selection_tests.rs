// These are certified Object records from the real converter, not Fixture identities.
// Selection has no near-window input: distance cannot disable a ready object batch.
use super::*;

fn assert_objects_selected_at_all_distances(is_tree: bool) {
    for cell in [
        IVec2::ZERO,
        IVec2::X,
        IVec2::new(2, -2),
        IVec2::new(64, -128),
        IVec2::new(-128, 64),
    ] {
        let directory =
            crate::cluster::reader::producer_fixture_tests::published_for_all_distance_selection(
                is_tree, cell,
            );
        let certificate =
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap();
        let records: Vec<_> = certificate.records().collect();
        assert_eq!(
            records.len(),
            2,
            "both placing worlds must certify at {cell:?}"
        );
        let placed: Vec<_> = records
            .iter()
            .map(|(key, record)| {
                let source = certificate.placed_reference(record.reference).unwrap();
                assert_eq!(source.is_tree, is_tree);
                assert_eq!(source.visible_when_distant, !is_tree);
                assert_eq!(source.base_has_distant_lod, Some(true));
                assert_eq!(source.snapshot.cell, (cell.x, cell.y));
                assert!(certificate.reference_complete(record.reference));
                ClusterInstance {
                    identity: ClusterInstanceIdentity::Object(**key),
                    mesh: 0,
                    batch: 0,
                    world_from_local: Mat4::IDENTITY,
                    source_worldspace: record.worldspace,
                }
            })
            .collect();
        let data = SelectionData {
            identity: 1,
            placed: &placed,
            batch_count: 1,
            certificate: Some(&certificate),
            terrain_blocks: &[],
            capacity: placed.len() as u32,
            // Textureless inputs do not need tangents. The independent tangent gate stays intact.
            meshes: &[],
        };
        for (_, record) in &records {
            let world = Some(record.worldspace);
            let mut index = SelectionIndex::new(data.identity);
            index.append(data, world);
            assert!(index.batches[0].fixtures.is_empty());
            assert_eq!(index.batches[0].objects.len(), 1);
            let inputs = BaseInputs {
                gpu: data.identity,
                world,
                drawable: true,
            };
            let ready = BatchState {
                ready: true,
                template: true,
                normal_mapped: false,
            };
            // The cluster path is active, with no streaming window/policy resource installed.
            let mut base = SelectionBase::build(&data, &index, inputs, vec![ready]);
            let assert_mask = |base: &SelectionBase, enabled: bool| {
                for (at, instance) in placed.iter().enumerate() {
                    assert_eq!(
                        base.mask.get(at as u32),
                        enabled && instance.identity.placed_worldspace(Some(&certificate)) == world,
                        "TREE={is_tree}, cell={cell:?}, world={world:?}, instance={at}"
                    );
                }
            };
            assert_mask(&base, true);
            for state in [
                BatchState {
                    ready: false,
                    ..ready
                },
                ready,
                BatchState {
                    template: false,
                    ..ready
                },
                ready,
            ] {
                base = base.update(&data, &index, inputs, vec![state]);
                assert_mask(&base, state.ready && state.template);
            }
            base = base.update(
                &data,
                &index,
                BaseInputs {
                    drawable: false,
                    ..inputs
                },
                vec![ready],
            );
            assert_mask(&base, false);
            base = base.update(&data, &index, inputs, vec![ready]);
            assert_mask(&base, true);
        }
    }
}

#[test]
fn certified_tree_objects_are_selected_at_all_distances_without_native_policy() {
    assert_objects_selected_at_all_distances(true);
}

#[test]
fn certified_visible_when_distant_objects_are_selected_at_all_distances_without_native_policy() {
    assert_objects_selected_at_all_distances(false);
}
