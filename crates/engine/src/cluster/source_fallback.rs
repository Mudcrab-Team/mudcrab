//! Negative source qualification asks the ordinary loader to retain exact references.
//! This resource supplies demand only: membership never proves visibility or draw readiness.

use super::reader::CertifiedPackage;
use bevy::prelude::Resource;
use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
};

/// Demand belongs to one live certificate allocation, including its exact validated source scope.
/// A weak owner cannot prolong an old package's dataset lock after publication is withdrawn.
#[derive(Resource, Default)]
pub(crate) struct SourceFallbackDemand {
    certificate: Option<Weak<CertifiedPackage>>,
    revision: u64,
    refs: BTreeSet<u32>,
    fresh: bool,
}

impl SourceFallbackDemand {
    /// Whether a publication attempt still needs its one-use acknowledgement.
    pub(crate) fn has_fresh(&self) -> bool {
        self.fresh
    }

    /// Publish the exact negative set; an absent package always withdraws all references.
    /// Equal sets under a different certificate allocation are still a new publication.
    pub(crate) fn update(
        &mut self,
        package: Option<&Arc<CertifiedPackage>>,
        mut refs: BTreeSet<u32>,
    ) -> bool {
        // Fresh feedback is required even when its certificate and negative set are unchanged.
        // Freshness is one-use delivery state, not a new demand revision or draw permission.
        self.fresh = true;
        if package.is_none() {
            refs.clear();
        }
        let same_certificate = match (&self.certificate, package) {
            (Some(previous), Some(current)) => previous
                .upgrade()
                .is_some_and(|previous| Arc::ptr_eq(&previous, current)),
            (None, None) => true,
            _ => false,
        };
        if same_certificate && self.refs == refs {
            return false;
        }
        self.revision = self
            .revision
            .checked_add(1)
            .expect("source fallback demand revision exhausted");
        self.certificate = package.map(Arc::downgrade);
        self.refs = refs;
        true
    }

    /// Inspect retained demand for the same live allocation without consuming its freshness.
    /// Hashes and paths are insufficient.
    pub(crate) fn for_package(
        &self,
        package: &Arc<CertifiedPackage>,
    ) -> Option<(u64, &BTreeSet<u32>)> {
        let certificate = self.certificate.as_ref()?.upgrade()?;
        Arc::ptr_eq(&certificate, package).then_some((self.revision, &self.refs))
    }

    /// Redeliver unchanged demand without cloning its set or advancing its revision.
    pub(crate) fn refresh_for_package(&mut self, package: &Arc<CertifiedPackage>) -> bool {
        if self.for_package(package).is_none() {
            return false;
        }
        self.fresh = true;
        true
    }

    /// Apply disjoint reference deltas to retained demand for this allocation.
    /// The caller supplies a full publication after withdrawal or replacement.
    pub(crate) fn update_delta(
        &mut self,
        package: &Arc<CertifiedPackage>,
        added: &BTreeSet<u32>,
        removed: &BTreeSet<u32>,
    ) -> bool {
        if self.for_package(package).is_none() {
            return false;
        }
        self.fresh = true;
        let mut changed = false;
        for reference in removed {
            changed |= self.refs.remove(reference);
        }
        for reference in added {
            changed |= self.refs.insert(*reference);
        }
        if changed {
            self.revision = self
                .revision
                .checked_add(1)
                .expect("source fallback demand revision exhausted");
        }
        changed
    }

    /// Consume one publication attempt, including attempts for the wrong or expired certificate.
    /// Absence means no fresh feedback, never a proven empty fallback set or draw readiness.
    pub(crate) fn consume_for_package(
        &mut self,
        package: &Arc<CertifiedPackage>,
    ) -> Option<(u64, &BTreeSet<u32>)> {
        let fresh = std::mem::replace(&mut self.fresh, false);
        if !fresh {
            return None;
        }
        self.for_package(package)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::reader::producer_fixture_tests::published_for_loader;

    fn references(package: &CertifiedPackage) -> BTreeSet<u32> {
        package
            .records()
            .map(|(_, record)| record.reference)
            .collect()
    }

    #[test]
    fn cloned_certificate_reuses_unchanged_demand_and_only_set_changes_advance_revision() {
        let directory = published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let clone = package.clone();
        let all = references(&package);
        assert!(all.len() >= 2);
        let one = BTreeSet::from([*all.first().unwrap()]);
        let mut demand = SourceFallbackDemand::default();
        assert!(demand.for_package(&package).is_none());
        assert!(demand.update(Some(&package), all.clone()));
        assert_eq!(demand.for_package(&clone), Some((1, &all)));
        assert!(!demand.update(Some(&clone), all.clone()));
        assert_eq!(demand.for_package(&package), Some((1, &all)));
        assert!(demand.update(Some(&clone), one.clone()));
        assert_eq!(demand.for_package(&package), Some((2, &one)));
        assert!(!demand.update(Some(&package), one.clone()));
        assert_eq!(demand.for_package(&clone), Some((2, &one)));
        assert!(demand.update(Some(&package), BTreeSet::new()));
        assert_eq!(demand.for_package(&clone), Some((3, &BTreeSet::new())));
    }

    #[test]
    fn consuming_requires_fresh_feedback_and_identical_republication_keeps_the_revision() {
        let directory = published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let clone = package.clone();
        let refs = references(&package);
        let mut demand = SourceFallbackDemand::default();
        assert!(!demand.fresh);
        assert!(!demand.refresh_for_package(&package));
        assert!(demand.consume_for_package(&package).is_none());
        assert!(demand.update(Some(&package), refs.clone()));
        assert_eq!(demand.for_package(&package), Some((1, &refs)));
        assert_eq!(demand.consume_for_package(&clone), Some((1, &refs)));
        assert!(!demand.fresh);
        assert!(demand.consume_for_package(&package).is_none());
        assert_eq!(demand.for_package(&package), Some((1, &refs)));
        assert!(demand.refresh_for_package(&clone));
        assert_eq!(demand.for_package(&package), Some((1, &refs)));
        assert_eq!(demand.consume_for_package(&package), Some((1, &refs)));
        assert!(demand.consume_for_package(&clone).is_none());
        assert!(!demand.update(Some(&clone), refs.clone()));
        assert!(demand.fresh);
        assert_eq!(demand.consume_for_package(&package), Some((1, &refs)));
        assert!(demand.consume_for_package(&clone).is_none());
    }

    #[test]
    fn deltas_preserve_full_publication_revisions_and_allocation_scope() {
        let directory = published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let other = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let refs = references(&package);
        let one = BTreeSet::from([*refs.first().unwrap()]);
        let empty = BTreeSet::new();
        let mut incremental = SourceFallbackDemand::default();
        let mut full = SourceFallbackDemand::default();
        incremental.update(Some(&package), refs.clone());
        full.update(Some(&package), refs.clone());
        assert!(incremental.consume_for_package(&package).is_some());
        assert!(!incremental.update_delta(&other, &empty, &refs));
        assert!(incremental.consume_for_package(&package).is_none());
        assert!(incremental.update_delta(&package, &empty, &one));
        let remaining = refs.difference(&one).copied().collect();
        full.update(Some(&package), remaining);
        assert_eq!(
            incremental.consume_for_package(&package),
            full.consume_for_package(&package)
        );
        assert!(!incremental.update_delta(&package, &empty, &one));
        full.refresh_for_package(&package);
        assert_eq!(
            incremental.consume_for_package(&package),
            full.consume_for_package(&package)
        );
        assert!(incremental.update_delta(&package, &one, &empty));
        full.update(Some(&package), refs);
        assert_eq!(
            incremental.consume_for_package(&package),
            full.consume_for_package(&package)
        );
    }

    #[test]
    fn independent_opens_of_identical_content_require_a_new_publication() {
        let directory = published_for_loader();
        let first = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let second = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(
            first.envelope().core_identity,
            second.envelope().core_identity
        );
        assert_eq!(
            first.envelope().builder_identity,
            second.envelope().builder_identity
        );
        let refs = references(&first);
        assert_eq!(refs, references(&second));
        let mut demand = SourceFallbackDemand::default();
        assert!(demand.update(Some(&first), refs.clone()));
        assert!(demand.for_package(&second).is_none());
        assert!(demand.consume_for_package(&second).is_none());
        assert!(!demand.fresh);
        assert!(!demand.refresh_for_package(&second));
        assert!(!demand.fresh);
        assert!(demand.consume_for_package(&first).is_none());
        assert!(demand.update(Some(&second), refs.clone()));
        assert!(demand.for_package(&first).is_none());
        assert_eq!(demand.for_package(&second), Some((2, &refs)));
        assert_eq!(demand.consume_for_package(&second), Some((2, &refs)));
    }

    #[test]
    fn withdrawal_clears_even_supplied_references_and_repeated_absence_is_unchanged() {
        let directory = published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let refs = references(&package);
        assert!(!refs.is_empty());
        let mut demand = SourceFallbackDemand::default();
        assert!(!demand.update(None, refs.clone()));
        assert_eq!(demand.revision, 0);
        assert!(demand.refs.is_empty());
        assert!(demand.update(Some(&package), refs.clone()));
        assert!(demand.update(None, refs.clone()));
        assert_eq!(demand.revision, 2);
        assert!(demand.refs.is_empty());
        assert!(demand.certificate.is_none());
        assert!(demand.for_package(&package).is_none());
        assert!(!demand.update(None, refs.clone()));
        assert_eq!(demand.revision, 2);
        assert!(demand.update(Some(&package), refs.clone()));
        assert_eq!(demand.for_package(&package), Some((3, &refs)));
    }

    #[test]
    fn expired_weak_certificate_neither_retains_the_package_nor_matches_another_open() {
        let directory = published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let weak = Arc::downgrade(&package);
        let refs = references(&package);
        let mut demand = SourceFallbackDemand::default();
        assert!(demand.update(Some(&package), refs.clone()));
        assert_eq!(Arc::strong_count(&package), 1);
        drop(package);
        assert!(weak.upgrade().is_none());
        assert!(demand.certificate.as_ref().unwrap().upgrade().is_none());
        let replacement = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        assert!(demand.for_package(&replacement).is_none());
        assert!(demand.consume_for_package(&replacement).is_none());
        assert!(!demand.fresh);
        assert!(!demand.refresh_for_package(&replacement));
        assert!(!demand.fresh);
        assert!(demand.update(Some(&replacement), refs.clone()));
        assert_eq!(demand.for_package(&replacement), Some((2, &refs)));
        assert_eq!(demand.consume_for_package(&replacement), Some((2, &refs)));
    }
}
