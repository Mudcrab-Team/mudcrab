//! Launch-time primitive ownership; no mesh handles or paths participate in matching.

use super::reader::{CertifiedPackage, RecordKey};
use bevy::prelude::*;

#[path = "ownership_template.rs"]
mod template;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};
pub(crate) use template::OwnedPrimitiveSource;

/// One live certificate's identity index and terminal batch qualifications.
/// Missing batch entries are pending, never an implicit permission to hide.
#[derive(Resource, Default)]
pub(crate) struct OwnershipFacts {
    certificate: Option<Arc<CertifiedPackage>>,
    references: HashMap<u32, Vec<RecordKey>>,
    source_fallback: BTreeMap<RecordKey, bool>,
    /// First terminal material/template qualification for this certificate.
    /// Runtime withdrawal and failed resources cannot change ownership.
    terminal: BTreeMap<RecordKey, bool>,
    terminal_revision: u64,
    refused_records: BTreeSet<RecordKey>,
    refused_references: BTreeSet<u32>,
    refusal_revision: u64,
}

impl OwnershipFacts {
    #[cfg(test)]
    pub(crate) fn publish(
        &mut self,
        package: &Arc<CertifiedPackage>,
        records: BTreeMap<RecordKey, bool>,
    ) {
        self.publish_updates(
            package,
            true,
            records.into_iter().map(|(key, value)| (key, Some(value))),
        );
    }

    /// Update the live projection while freezing each record's first terminal
    /// qualification. Only replacing the certificate replaces ownership.
    pub(crate) fn publish_updates(
        &mut self,
        package: &Arc<CertifiedPackage>,
        reset: bool,
        records: impl IntoIterator<Item = (RecordKey, Option<bool>)>,
    ) {
        if !self
            .certificate
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, package))
        {
            self.source_fallback.clear();
            self.terminal.clear();
            self.refused_records.clear();
            self.refused_references.clear();
            self.refusal_revision = self.refusal_revision.wrapping_add(1);
            self.terminal_revision = self.terminal_revision.wrapping_add(1);
            self.references.clear();
            for (key, identity) in package.records() {
                self.references
                    .entry(identity.reference)
                    .or_default()
                    .push(*key);
            }
            self.certificate = Some(package.clone());
        }
        if reset {
            self.source_fallback.clear();
        }
        let mut added_terminal = false;
        let mut added_refusal = false;
        for (key, value) in records {
            if let Some(value) = value {
                if let std::collections::btree_map::Entry::Vacant(entry) = self.terminal.entry(key)
                {
                    entry.insert(value);
                    added_terminal = true;
                    if value {
                        self.refused_records.insert(key);
                        if let Some(identity) = package.record(&key) {
                            self.refused_references.insert(identity.reference);
                        }
                        added_refusal = true;
                    }
                }
                self.source_fallback.insert(key, value);
            } else {
                self.source_fallback.remove(&key);
            }
        }
        if added_refusal {
            self.refusal_revision = self.refusal_revision.wrapping_add(1);
        }
        if added_terminal {
            self.terminal_revision = self.terminal_revision.wrapping_add(1);
        }
    }

    pub(crate) fn revoke(&mut self) {
        // Losing runtime qualification is not a new certificate and cannot
        // restore stock bodies. Preserve the occurrence index and frozen facts.
        self.source_fallback.clear();
    }

    pub(crate) fn package_matches(&self, package: &Arc<CertifiedPackage>) -> bool {
        self.certificate
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, package))
    }

    #[cfg(test)]
    pub(crate) fn terminal_revision(&self) -> u64 {
        self.terminal_revision
    }

    /// Changes only when permanent stock permission changes, or the certificate is replaced.
    /// New owned facts do not invalidate refusal-only projections.
    pub(crate) fn refusal_revision(&self) -> u64 {
        self.refusal_revision
    }

    pub(crate) fn permanent_refused_records(&self) -> BTreeSet<RecordKey> {
        self.refused_records.clone()
    }

    pub(crate) fn permanent_refused_references(&self) -> BTreeSet<u32> {
        self.refused_references.clone()
    }

    #[cfg(test)]
    pub(crate) fn permanent_refused_ordinals(&self, reference: u32) -> BTreeSet<u32> {
        self.ordinals_with_decision(reference, true)
    }

    fn ordinals_with_decision(&self, reference: u32, refused: bool) -> BTreeSet<u32> {
        let Some(package) = self.certificate.as_ref() else {
            return BTreeSet::new();
        };
        self.references
            .get(&reference)
            .into_iter()
            .flatten()
            .filter(|key| self.terminal.get(key) == Some(&refused))
            .filter_map(|key| package.record(key).map(|identity| identity.primitive))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn reference_has_owned(&self, reference: u32) -> bool {
        self.references
            .get(&reference)
            .into_iter()
            .flatten()
            .any(|key| self.terminal.get(key) == Some(&false))
    }

    pub(crate) fn owned_ordinals(&self, reference: u32) -> BTreeSet<u32> {
        self.ordinals_with_decision(reference, false)
    }

    pub(crate) fn can_spawn(&self, reference: u32) -> bool {
        self.certificate.is_some()
            && self
                .references
                .get(&reference)
                .into_iter()
                .flatten()
                .all(|key| self.terminal.contains_key(key))
    }

    #[cfg(test)]
    pub(crate) fn projected_records(&self) -> &BTreeMap<RecordKey, bool> {
        &self.source_fallback
    }
}
