//! Immutable instance membership scoped to one certificate and final loader layout.
//!
//! Membership includes pending instances and grants no drawability or replacement proof.

use std::{collections::HashMap, sync::Arc};

use super::{
    instance_identity::ClusterInstanceIdentity,
    reader::{CertifiedPackage, RecordKey},
    render::ClusterInstance,
};

/// Complete instance membership captured after the loader's final reordering.
///
/// Object keys belong to the retained certificate. Every occurrence remains in the
/// ordered snapshot and batch index, while inverse lists contain each batch once
/// in ascending order. Terrain retains its explicit world identity in the snapshot.
/// Mesh preparation, residency, refusal, and runtime state do not filter membership.
pub struct InstanceInventory {
    package: Arc<CertifiedPackage>,
    layout_scope: u64,
    instances: Vec<(ClusterInstanceIdentity, u32)>,
    by_batch: Vec<Vec<usize>>,
    by_record: HashMap<RecordKey, Vec<u32>>,
    by_reference: HashMap<u32, Vec<u32>>,
}

impl InstanceInventory {
    /// Captures all instances in their final scene order, including pending instances.
    ///
    /// The caller must supply the certificate that originally scoped the object keys
    /// and the layout scope shared with the scene's batches. Numeric keys alone
    /// cannot establish that an instance came from this package generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a batch outside `batch_count`, an object key absent from
    /// `package`, or a fixture identity in this certified inventory. Terrain identity
    /// is retained as supplied; validating terrain blocks belongs to the loader.
    pub fn build(
        package: Arc<CertifiedPackage>,
        layout_scope: u64,
        instances: &[ClusterInstance],
        batch_count: usize,
    ) -> Result<Self, String> {
        #[cfg(test)]
        crate::work_counters::record("cluster_inventory/index_build");
        let mut inventory = Self {
            package,
            layout_scope,
            instances: Vec::with_capacity(instances.len()),
            by_batch: vec![Vec::new(); batch_count],
            by_record: HashMap::new(),
            by_reference: HashMap::new(),
        };
        #[cfg(test)]
        crate::work_counters::record_count(
            "cluster_inventory/index_instances",
            instances.len() as u64,
        );
        for (index, instance) in instances.iter().enumerate() {
            let batch = instance.batch;
            let indices = inventory.by_batch.get_mut(batch as usize).ok_or_else(|| {
                format!("cluster instance {index} batch {batch} is outside {batch_count} batches")
            })?;
            match instance.identity {
                ClusterInstanceIdentity::Object(key) => {
                    let record = inventory.package.record(&key).ok_or_else(|| {
                        format!(
                            "cluster instance {index} object key {key:?} is outside its certificate"
                        )
                    })?;
                    inventory.by_record.entry(key).or_default().push(batch);
                    inventory
                        .by_reference
                        .entry(record.reference)
                        .or_default()
                        .push(batch);
                }
                ClusterInstanceIdentity::Terrain { .. } => {}
                ClusterInstanceIdentity::Fixture => {
                    return Err(format!(
                        "cluster instance {index} has a fixture identity in a certified inventory"
                    ));
                }
            }
            indices.push(index);
            inventory.instances.push((instance.identity, batch));
        }
        for batches in inventory
            .by_record
            .values_mut()
            .chain(inventory.by_reference.values_mut())
        {
            batches.sort_unstable();
            batches.dedup();
        }
        bevy::log::debug!(
            layout_scope,
            index_builds = 1,
            instance_visits = instances.len(),
            batch_count,
            "cluster instance inventory built"
        );
        Ok(inventory)
    }

    /// Tests certificate allocation identity and the loader's batch layout scope.
    pub fn matches(&self, package: &Arc<CertifiedPackage>, layout_scope: u64) -> bool {
        Arc::ptr_eq(&self.package, package) && self.layout_scope == layout_scope
    }

    /// Tests the ordered identity and batch snapshot after a structural scene change.
    ///
    /// This walks instances and must not be used as a settled-frame freshness check.
    /// Transforms, meshes, and the legacy source-world tag are not membership inputs.
    pub fn matches_instances(&self, instances: &[ClusterInstance]) -> bool {
        if self.instances.len() != instances.len() {
            return false;
        }
        #[cfg(test)]
        let mut visits = 0;
        let matches = self
            .instances
            .iter()
            .zip(instances)
            .all(|(&(identity, batch), instance)| {
                #[cfg(test)]
                {
                    visits += 1;
                }
                identity == instance.identity && batch == instance.batch
            });
        #[cfg(test)]
        crate::work_counters::record_count("cluster_inventory/validation_instances", visits);
        matches
    }

    /// Returns all occurrence indices for a batch in scene order, or an empty slice.
    ///
    /// Consumers count entries they traverse at their own invalidation event.
    pub fn indices_for_batch(&self, batch: u32) -> &[usize] {
        self.by_batch
            .get(batch as usize)
            .map_or(&[][..], Vec::as_slice)
    }

    /// Returns the distinct batches containing this package-local object key.
    pub fn batches_for_record(&self, record: &RecordKey) -> &[u32] {
        self.by_record.get(record).map_or(&[], Vec::as_slice)
    }

    /// Returns the distinct batches containing any mapped record of this reference.
    ///
    /// Unknown and wholly unclustered references have no instance membership.
    pub fn batches_for_reference(&self, reference: u32) -> &[u32] {
        self.by_reference.get(&reference).map_or(&[], Vec::as_slice)
    }

    /// Returns the layout's batch count, including batches with no instances.
    pub fn batch_count(&self) -> usize {
        self.by_batch.len()
    }

    /// Returns the ordered identity and batch snapshot, including every occurrence.
    pub fn instances(&self) -> &[(ClusterInstanceIdentity, u32)] {
        &self.instances
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Mat4;

    fn certified() -> (tempfile::TempDir, Arc<CertifiedPackage>) {
        let directory = super::super::reader::producer_fixture_tests::published_for_loader();
        let package =
            Arc::new(super::super::overlay_fixture::open_fixture(directory.path(), false).unwrap());
        (directory, package)
    }

    fn instance(identity: ClusterInstanceIdentity, batch: u32) -> ClusterInstance {
        ClusterInstance {
            identity,
            mesh: 0,
            batch,
            world_from_local: Mat4::IDENTITY,
            // Deliberately unrelated to the certified or terrain world identity.
            source_worldspace: u32::MAX,
        }
    }

    #[test]
    fn retains_all_occurrences_without_mesh_preparation_and_deduplicates_inverse_batches() {
        let (_directory, package) = certified();
        let key = *package.records().next().unwrap().0;
        let reference = package.record(&key).unwrap().reference;
        let object = ClusterInstanceIdentity::Object(key);
        let instances = [2, 0, 2, 1, 0].map(|batch| instance(object, batch));
        // No meshes or upload state exist here: pending instances remain dependencies.
        let inventory = InstanceInventory::build(package, 7, &instances, 4).unwrap();

        assert_eq!(inventory.indices_for_batch(0), &[1, 4]);
        assert_eq!(inventory.indices_for_batch(1), &[3]);
        assert_eq!(inventory.indices_for_batch(2), &[0, 2]);
        assert!(inventory.indices_for_batch(3).is_empty());
        assert!(inventory.indices_for_batch(u32::MAX).is_empty());
        assert_eq!(inventory.batches_for_record(&key), &[0, 1, 2]);
        assert_eq!(inventory.batches_for_reference(reference), &[0, 1, 2]);
        assert_eq!(inventory.batch_count(), 4);
        assert_eq!(
            inventory.instances(),
            &[
                (object, 2),
                (object, 0),
                (object, 2),
                (object, 1),
                (object, 0)
            ]
        );
        assert!(inventory.batches_for_reference(u32::MAX).is_empty());
        let absent = RecordKey {
            ordinal: u32::MAX,
            ..key
        };
        assert!(inventory.batches_for_record(&absent).is_empty());
    }

    #[test]
    fn preserves_object_and_terrain_worlds_sharing_one_batch() {
        let (_directory, package) = certified();
        let keys: Vec<_> = package.records().map(|(key, _)| *key).collect();
        assert!(keys.len() >= 2);
        let terrain = ClusterInstanceIdentity::Terrain {
            block: 3,
            worldspace: u32::MAX - 1,
            tier: 2,
            anchor: [-8, 16],
        };
        let instances = [
            instance(ClusterInstanceIdentity::Object(keys[0]), 0),
            instance(terrain, 0),
            instance(ClusterInstanceIdentity::Object(keys[1]), 0),
        ];
        let inventory = InstanceInventory::build(package.clone(), 8, &instances, 1).unwrap();

        assert_eq!(inventory.indices_for_batch(0), &[0, 1, 2]);
        assert_eq!(inventory.instances()[1], (terrain, 0));
        for key in &keys[..2] {
            assert_eq!(inventory.batches_for_record(key), &[0]);
            let record = package.record(key).unwrap();
            assert_eq!(inventory.batches_for_reference(record.reference), &[0]);
        }
        assert!(inventory.batches_for_reference(u32::MAX - 1).is_empty());
    }

    #[test]
    fn provenance_requires_the_same_allocation_and_layout_even_for_equal_package_bytes() {
        let (directory, package) = certified();
        let inventory = InstanceInventory::build(package.clone(), 9, &[], 0).unwrap();
        let reopened =
            Arc::new(super::super::overlay_fixture::open_fixture(directory.path(), false).unwrap());

        assert!(inventory.matches(&package.clone(), 9));
        assert!(!inventory.matches(&package, 10));
        assert!(!inventory.matches(&reopened, 9));
        assert!(inventory.matches_instances(&[]));
        assert_eq!(inventory.batch_count(), 0);
    }

    #[test]
    fn snapshot_detects_reorder_batch_identity_and_length_changes() {
        let (_directory, package) = certified();
        let key = *package.records().next().unwrap().0;
        let object = ClusterInstanceIdentity::Object(key);
        let original = [instance(object, 0), instance(object, 1)];
        let inventory = InstanceInventory::build(package, 10, &original, 2).unwrap();

        assert!(inventory.matches_instances(&original));
        let mut changed = original;
        changed.swap(0, 1);
        assert!(!inventory.matches_instances(&changed));
        changed = original;
        changed[0].batch = 1;
        assert!(!inventory.matches_instances(&changed));
        changed = original;
        changed[0].identity = ClusterInstanceIdentity::Fixture;
        assert!(!inventory.matches_instances(&changed));
        assert!(!inventory.matches_instances(&original[..1]));
        changed = original;
        changed[0].mesh = 999;
        changed[0].world_from_local = Mat4::ZERO;
        changed[0].source_worldspace = 0;
        assert!(inventory.matches_instances(&changed));
    }

    #[test]
    fn rejects_unknown_objects_fixture_identities_and_out_of_range_batches() {
        let (_directory, package) = certified();
        let key = *package.records().next().unwrap().0;
        let absent = RecordKey {
            ordinal: u32::MAX,
            ..key
        };
        for (identity, batch, batch_count, reason) in [
            (ClusterInstanceIdentity::Object(absent), 0, 1, "certificate"),
            (ClusterInstanceIdentity::Fixture, 0, 1, "fixture identity"),
            (
                ClusterInstanceIdentity::Object(key),
                1,
                1,
                "outside 1 batches",
            ),
            (
                ClusterInstanceIdentity::Object(key),
                0,
                0,
                "outside 0 batches",
            ),
        ] {
            let error = InstanceInventory::build(
                package.clone(),
                11,
                &[instance(identity, batch)],
                batch_count,
            )
            .err()
            .expect("invalid membership must fail");
            assert!(error.contains(reason), "{error}");
        }
    }
}
