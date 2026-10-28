//! Shared-resource reservations with explicit owner and disposal lifetimes.
//!
//! Dropping the last owner does not prove that a loader, render world or GPU has
//! disposed of a resource. Such orphan reservations remain until the caller
//! confirms absence. Transient bytes are additional to resident bytes.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceReservation {
    pub key: String,
    pub resident_bytes: u64,
    pub transient_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReservationTotals {
    pub resident_bytes: u64,
    pub transient_bytes: u64,
    pub orphan_resident_bytes: u64,
    pub orphan_transient_bytes: u64,
    pub resources: usize,
    pub owners: usize,
    pub orphan_resources: usize,
    pub denied_total: u64,
    pub overflow_total: u64,
}

impl ReservationTotals {
    pub fn peak_bytes(self) -> Option<u64> {
        self.resident_bytes.checked_add(self.transient_bytes)
    }

    pub fn orphan_bytes(self) -> Option<u64> {
        self.orphan_resident_bytes
            .checked_add(self.orphan_transient_bytes)
    }
}

#[derive(Debug, Default)]
struct ResourceRecord {
    resident_bytes: u64,
    transient_estimate: u64,
    transient_charged: bool,
    /// Value is whether this owner's use has completed preparation.
    owners: BTreeMap<String, bool>,
    unprepared_owners: usize,
}

impl ResourceRecord {
    fn charged_transient(&self) -> u64 {
        if self.transient_charged {
            self.transient_estimate
        } else {
            0
        }
    }
}

#[derive(Debug)]
struct ProjectedResource {
    key: String,
    resident_bytes: u64,
    transient_estimate: u64,
    transient_charged: bool,
    owners: usize,
    unprepared_owners: usize,
}

impl ProjectedResource {
    fn charged_transient(&self) -> u64 {
        if self.transient_charged {
            self.transient_estimate
        } else {
            0
        }
    }
}

/// The caller chooses resource identities, including pack/content identity and
/// unique keys for placement/cell allocations. This ledger performs no I/O.
#[derive(Debug, Default)]
pub struct MemoryLedger {
    resources: BTreeMap<String, ResourceRecord>,
    owners: BTreeMap<String, BTreeSet<String>>,
    totals: ReservationTotals,
}

impl MemoryLedger {
    pub fn totals(&self) -> ReservationTotals {
        self.totals
    }

    /// Atomically extend one owner's bundle. Shared keys count once;
    /// duplicate keys in a bundle take the maximum of each byte estimate.
    /// `limit == 0` disables the byte limit, but arithmetic is still checked.
    /// A retained owner may reconcile without growing an already over-limit ledger.
    /// Failed admission changes only the denial/overflow counters.
    pub fn reserve(&mut self, owner: String, bundle: Vec<ResourceReservation>, limit: u64) -> bool {
        if owner.is_empty() || bundle.iter().any(|cost| cost.key.is_empty()) {
            return self.deny(false);
        }
        let mut unique = BTreeMap::<String, (u64, u64)>::new();
        for cost in bundle {
            let entry = unique.entry(cost.key).or_default();
            entry.0 = entry.0.max(cost.resident_bytes);
            entry.1 = entry.1.max(cost.transient_bytes);
            if entry.0.checked_add(entry.1).is_none() {
                return self.deny(true);
            }
        }
        // Callers can extend a cell owner with discovered surface textures.
        // Existing ownership is removed only by explicit abandon(), never by a
        // later reservation that contains only newly discovered dependencies.
        if let Some(previous) = self.owners.get(&owner) {
            for key in previous {
                let record = &self.resources[key];
                unique
                    .entry(key.clone())
                    .or_insert((record.resident_bytes, record.transient_estimate));
            }
        }
        let affected: BTreeSet<String> = unique.keys().cloned().collect();
        let mut next = self.totals;
        // Remove every old contribution before adding replacements, so a valid
        // final total cannot overflow merely because keys sort in another order.
        for key in &affected {
            if let Some(previous) = self.resources.get(key) {
                next.resident_bytes -= previous.resident_bytes;
                next.transient_bytes -= previous.charged_transient();
                if previous.owners.is_empty() {
                    next.orphan_resident_bytes -= previous.resident_bytes;
                    next.orphan_transient_bytes -= previous.charged_transient();
                    next.orphan_resources -= 1;
                }
            }
        }
        let mut projected = Vec::with_capacity(affected.len());
        for key in affected {
            let previous = self.resources.get(&key);
            let incoming = unique.get(&key).copied();
            let previous_owner = previous
                .and_then(|record| record.owners.get(&owner))
                .copied();
            let mut owners = previous.map_or(0, |record| record.owners.len());
            let mut unprepared = previous.map_or(0, |record| record.unprepared_owners);
            match (previous_owner, incoming) {
                (None, Some(_)) => {
                    owners += 1;
                    unprepared += 1;
                }
                (Some(prepared), None) => {
                    owners -= 1;
                    if !prepared {
                        unprepared -= 1;
                    }
                }
                _ => {}
            }
            let resident = previous
                .map_or(0, |record| record.resident_bytes)
                .max(incoming.map_or(0, |cost| cost.0));
            let transient = previous
                .map_or(0, |record| record.transient_estimate)
                .max(incoming.map_or(0, |cost| cost.1));
            if resident.checked_add(transient).is_none() {
                return self.deny(true);
            }
            // An abandoned in-flight resource keeps its scratch reservation.
            // Owners that remain can establish shared preparation completion.
            let transient_charged = if owners == 0 {
                previous.is_some_and(|record| record.transient_charged)
            } else {
                unprepared > 0
            };
            let candidate = ProjectedResource {
                key,
                resident_bytes: resident,
                transient_estimate: transient,
                transient_charged,
                owners,
                unprepared_owners: unprepared,
            };
            if previous.is_none() {
                next.resources += 1;
            }
            let Some(new_resident) = next.resident_bytes.checked_add(candidate.resident_bytes)
            else {
                return self.deny(true);
            };
            let Some(new_transient) = next
                .transient_bytes
                .checked_add(candidate.charged_transient())
            else {
                return self.deny(true);
            };
            next.resident_bytes = new_resident;
            next.transient_bytes = new_transient;
            if candidate.owners == 0 {
                let Some(orphan_resident) = next
                    .orphan_resident_bytes
                    .checked_add(candidate.resident_bytes)
                else {
                    return self.deny(true);
                };
                let Some(orphan_transient) = next
                    .orphan_transient_bytes
                    .checked_add(candidate.charged_transient())
                else {
                    return self.deny(true);
                };
                next.orphan_resident_bytes = orphan_resident;
                next.orphan_transient_bytes = orphan_transient;
                next.orphan_resources += 1;
            }
            projected.push(candidate);
        }
        let Some(next_peak) = next.peak_bytes() else {
            return self.deny(true);
        };
        let current_peak = self.totals.peak_bytes().expect("ledger peak invariant");
        if limit != 0 && next_peak > limit && next_peak > current_peak {
            return self.deny(false);
        }
        let had_owner = self.owners.contains_key(&owner);
        let has_owner = !unique.is_empty();
        if had_owner && !has_owner {
            next.owners -= 1;
        } else if !had_owner && has_owner {
            next.owners += 1;
        }
        for candidate in projected {
            let record = self.resources.entry(candidate.key.clone()).or_default();
            record.resident_bytes = candidate.resident_bytes;
            record.transient_estimate = candidate.transient_estimate;
            record.transient_charged = candidate.transient_charged;
            record.unprepared_owners = candidate.unprepared_owners;
            if unique.contains_key(&candidate.key) {
                record.owners.entry(owner.clone()).or_insert(false);
            } else {
                record.owners.remove(&owner);
            }
        }
        if has_owner {
            self.owners.insert(owner, unique.into_keys().collect());
        } else {
            self.owners.remove(&owner);
        }
        self.totals = next;
        true
    }

    /// Release additional transient headroom only when every live owner of a
    /// shared resource has completed preparation. Resident charge is retained.
    pub fn mark_prepared(&mut self, owner: &str) -> bool {
        let Some(keys) = self.owners.get(owner) else {
            return false;
        };
        for key in keys {
            let record = self.resources.get_mut(key).expect("ledger owner invariant");
            let prepared = record
                .owners
                .get_mut(owner)
                .expect("ledger subscriber invariant");
            if !*prepared {
                *prepared = true;
                record.unprepared_owners -= 1;
            }
            if record.unprepared_owners == 0 && record.transient_charged {
                self.totals.transient_bytes -= record.transient_estimate;
                record.transient_charged = false;
            }
        }
        true
    }

    /// Remove one owner. Returned keys have just become orphaned and need
    /// loader/render/GPU absence confirmation before their charge can disappear.
    pub fn abandon(&mut self, owner: &str) -> Vec<String> {
        let Some(keys) = self.owners.remove(owner) else {
            return Vec::new();
        };
        self.totals.owners -= 1;
        let mut orphaned = Vec::new();
        for key in keys {
            let record = self
                .resources
                .get_mut(&key)
                .expect("ledger owner invariant");
            let prepared = record
                .owners
                .remove(owner)
                .expect("ledger subscriber invariant");
            if !prepared {
                record.unprepared_owners -= 1;
            }
            if record.owners.is_empty() {
                self.totals.orphan_resident_bytes += record.resident_bytes;
                self.totals.orphan_transient_bytes += record.charged_transient();
                self.totals.orphan_resources += 1;
                orphaned.push(key);
            } else if record.unprepared_owners == 0 && record.transient_charged {
                self.totals.transient_bytes -= record.transient_estimate;
                record.transient_charged = false;
            }
        }
        orphaned
    }

    /// Transfer one reservation claim after another owner has adopted the same
    /// resource. Dropping the final claim leaves an orphan charge, as abandon().
    pub fn detach_resource(&mut self, owner: &str, resource_key: &str) -> bool {
        let Some(keys) = self.owners.get_mut(owner) else {
            return false;
        };
        if !keys.remove(resource_key) {
            return false;
        }
        if keys.is_empty() {
            self.owners.remove(owner);
            self.totals.owners -= 1;
        }
        let record = self
            .resources
            .get_mut(resource_key)
            .expect("ledger owner invariant");
        let prepared = record
            .owners
            .remove(owner)
            .expect("ledger subscriber invariant");
        if !prepared {
            record.unprepared_owners -= 1;
        }
        if record.owners.is_empty() {
            self.totals.orphan_resident_bytes += record.resident_bytes;
            self.totals.orphan_transient_bytes += record.charged_transient();
            self.totals.orphan_resources += 1;
        } else if record.unprepared_owners == 0 && record.transient_charged {
            self.totals.transient_bytes -= record.transient_estimate;
            record.transient_charged = false;
        }
        true
    }

    /// The caller must establish actual resource disposal, not merely an absent
    /// subscriber or absent loader state. Live ownership always blocks release.
    pub fn confirm_absent(&mut self, resource_key: &str) -> bool {
        if !self
            .resources
            .get(resource_key)
            .is_some_and(|record| record.owners.is_empty())
        {
            return false;
        }
        let record = self
            .resources
            .remove(resource_key)
            .expect("ledger absence invariant");
        self.totals.resident_bytes -= record.resident_bytes;
        self.totals.transient_bytes -= record.charged_transient();
        self.totals.orphan_resident_bytes -= record.resident_bytes;
        self.totals.orphan_transient_bytes -= record.charged_transient();
        self.totals.resources -= 1;
        self.totals.orphan_resources -= 1;
        true
    }

    pub fn contains_resource(&self, resource_key: &str) -> bool {
        self.resources.contains_key(resource_key)
    }

    pub fn resource_has_owners(&self, resource_key: &str) -> bool {
        self.resources
            .get(resource_key)
            .is_some_and(|record| !record.owners.is_empty())
    }

    pub fn owner_resources(&self, owner: &str) -> Option<&BTreeSet<String>> {
        self.owners.get(owner)
    }

    pub fn orphan_keys(&self) -> impl Iterator<Item = &str> {
        self.resources
            .iter()
            .filter(|(_, record)| record.owners.is_empty())
            .map(|(key, _)| key.as_str())
    }

    fn deny(&mut self, overflow: bool) -> bool {
        self.totals.denied_total = self.totals.denied_total.saturating_add(1);
        if overflow {
            self.totals.overflow_total = self.totals.overflow_total.saturating_add(1);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(key: &str, resident: u64, transient: u64) -> ResourceReservation {
        ResourceReservation {
            key: key.to_owned(),
            resident_bytes: resident,
            transient_bytes: transient,
        }
    }

    #[test]
    fn shared_texture_counts_once_and_waits_for_every_owner_to_prepare() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve(
            "placement-a".to_owned(),
            vec![cost("a.glb", 100, 50), cost("shared.ktx2", 1000, 500)],
            2000
        ));
        assert!(ledger.reserve(
            "placement-b".to_owned(),
            vec![cost("b.glb", 200, 100), cost("shared.ktx2", 1000, 500)],
            2000
        ));
        assert_eq!(ledger.totals().resident_bytes, 1300);
        assert_eq!(ledger.totals().transient_bytes, 650);
        assert_eq!(ledger.totals().resources, 3);
        assert!(ledger.mark_prepared("placement-a"));
        assert_eq!(ledger.totals().transient_bytes, 600);
        assert!(ledger.mark_prepared("placement-b"));
        assert_eq!(ledger.totals().transient_bytes, 0);
        assert_eq!(ledger.totals().resident_bytes, 1300);
        assert!(!ledger.confirm_absent("shared.ktx2"));
    }

    #[test]
    fn placement_allocations_remain_distinct_while_geometry_is_shared() {
        let mut ledger = MemoryLedger::default();
        for owner in ["a", "b"] {
            assert!(ledger.reserve(
                owner.to_owned(),
                vec![
                    cost("model.glb", 100, 20),
                    cost(&format!("placement/{owner}/collision"), 50, 10)
                ],
                500
            ));
        }
        assert_eq!(ledger.totals().resident_bytes, 200);
        assert_eq!(ledger.totals().transient_bytes, 40);
        assert_eq!(ledger.totals().owners, 2);
    }

    #[test]
    fn abandoned_loading_resource_stays_charged_until_confirmed_absent() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("a".to_owned(), vec![cost("model.glb", 100, 50)], 150));
        assert_eq!(ledger.abandon("a"), vec!["model.glb"]);
        assert_eq!(ledger.totals().peak_bytes(), Some(150));
        assert_eq!(ledger.totals().orphan_bytes(), Some(150));
        assert_eq!(ledger.totals().owners, 0);
        assert!(ledger.abandon("a").is_empty());
        assert!(!ledger.reserve("b".to_owned(), vec![cost("other.glb", 1, 0)], 150));
        assert!(ledger.confirm_absent("model.glb"));
        assert_eq!(ledger.totals().peak_bytes(), Some(0));
        assert!(!ledger.confirm_absent("model.glb"));
        assert!(ledger.reserve("b".to_owned(), vec![cost("other.glb", 1, 0)], 150));
    }

    #[test]
    fn readoption_transfers_orphan_charge_without_doubling() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("old".to_owned(), vec![cost("shared", 100, 50)], 150));
        ledger.abandon("old");
        assert!(ledger.reserve("new".to_owned(), vec![cost("shared", 100, 50)], 150));
        assert_eq!(ledger.totals().peak_bytes(), Some(150));
        assert_eq!(ledger.totals().orphan_bytes(), Some(0));
        assert_eq!(ledger.totals().resources, 1);
        assert!(!ledger.confirm_absent("shared"));
    }

    #[test]
    fn same_owner_reconciliation_preserves_preparation_and_never_shrinks_costs() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("a".to_owned(), vec![cost("shared", 100, 50)], 150));
        ledger.mark_prepared("a");
        assert!(ledger.reserve(
            "a".to_owned(),
            vec![cost("shared", 10, 5), cost("shared", 20, 2)],
            50
        ));
        assert_eq!(ledger.totals().resident_bytes, 100);
        assert_eq!(ledger.totals().transient_bytes, 0);
        assert_eq!(ledger.totals().owners, 1);
        assert!(ledger.reserve("b".to_owned(), vec![cost("shared", 10, 5)], 150));
        assert_eq!(ledger.totals().transient_bytes, 50);
        ledger.mark_prepared("b");
        assert!(ledger.reserve("b".to_owned(), vec![cost("shared", 200, 70)], 300));
        assert_eq!(ledger.totals().resident_bytes, 200);
        assert_eq!(ledger.totals().transient_bytes, 0);
    }

    #[test]
    fn extending_owner_retains_old_resources_and_fails_atomically_when_it_cannot_fit() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("a".to_owned(), vec![cost("old", 100, 50)], 150));
        let before = ledger.totals();
        assert!(!ledger.reserve("a".to_owned(), vec![cost("new", 100, 50)], 200));
        assert_eq!(
            ledger.owner_resources("a").unwrap(),
            &BTreeSet::from(["old".to_owned()])
        );
        assert!(!ledger.contains_resource("new"));
        assert_eq!(ledger.totals().resident_bytes, before.resident_bytes);
        assert_eq!(ledger.totals().transient_bytes, before.transient_bytes);
        assert_eq!(ledger.totals().denied_total, before.denied_total + 1);
        assert!(ledger.reserve("a".to_owned(), vec![cost("new", 100, 50)], 300));
        assert!(ledger.orphan_keys().next().is_none());
        assert_eq!(ledger.totals().orphan_bytes(), Some(0));
        assert!(!ledger.confirm_absent("old"));
        assert_eq!(
            ledger.owner_resources("a").unwrap(),
            &BTreeSet::from(["old".to_owned(), "new".to_owned()])
        );
        assert_eq!(ledger.abandon("a"), vec!["new", "old"]);
        assert!(ledger.confirm_absent("old"));
        assert_eq!(ledger.totals().peak_bytes(), Some(150));
    }

    #[test]
    fn overflow_never_partially_changes_resource_or_owner_maps() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("a".to_owned(), vec![cost("a", u64::MAX - 10, 0)], 0));
        assert!(!ledger.reserve("b".to_owned(), vec![cost("b", 11, 0)], 0));
        assert_eq!(ledger.totals().resources, 1);
        assert_eq!(ledger.totals().owners, 1);
        assert_eq!(ledger.totals().overflow_total, 1);
        assert!(!ledger.reserve("a".to_owned(), vec![cost("a", u64::MAX, 1)], 0));
        assert_eq!(ledger.totals().resident_bytes, u64::MAX - 10);
        assert_eq!(ledger.totals().overflow_total, 2);
    }

    #[test]
    fn dropping_unprepared_owner_releases_shared_scratch_only_with_a_prepared_owner() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("ready".to_owned(), vec![cost("shared", 100, 50)], 150));
        assert!(ledger.reserve("waiting".to_owned(), vec![cost("shared", 100, 50)], 150));
        ledger.mark_prepared("ready");
        assert_eq!(ledger.totals().transient_bytes, 50);
        assert!(ledger.abandon("waiting").is_empty());
        assert_eq!(ledger.totals().transient_bytes, 0);
        assert_eq!(ledger.abandon("ready"), vec!["shared"]);
        assert_eq!(ledger.totals().orphan_bytes(), Some(100));
    }

    #[test]
    fn mixed_owner_lifecycles_keep_totals_and_reverse_ownership_consistent() {
        let mut ledger = MemoryLedger::default();
        let mut random = 0x9e3779b97f4a7c15_u64;
        for _ in 0..1000 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let owner = format!("owner-{}", random % 4);
            let resource = format!("resource-{}", (random >> 8) % 5);
            match (random >> 16) % 4 {
                0 => {
                    let extra = format!("placement-{owner}");
                    ledger.reserve(
                        owner,
                        vec![
                            cost(
                                &resource,
                                ((random >> 24) % 100) + 1,
                                ((random >> 32) % 100) + 1,
                            ),
                            cost(&extra, 10, 5),
                        ],
                        700,
                    );
                }
                1 => {
                    ledger.mark_prepared(&owner);
                }
                2 => {
                    ledger.abandon(&owner);
                }
                _ => {
                    ledger.confirm_absent(&resource);
                }
            }
            let totals = ledger.totals();
            assert_eq!(
                totals.resident_bytes,
                ledger
                    .resources
                    .values()
                    .map(|record| record.resident_bytes)
                    .sum::<u64>()
            );
            assert_eq!(
                totals.transient_bytes,
                ledger
                    .resources
                    .values()
                    .map(ResourceRecord::charged_transient)
                    .sum::<u64>()
            );
            assert_eq!(totals.resources, ledger.resources.len());
            assert_eq!(totals.owners, ledger.owners.len());
            let orphans: Vec<_> = ledger
                .resources
                .values()
                .filter(|record| record.owners.is_empty())
                .collect();
            assert_eq!(totals.orphan_resources, orphans.len());
            assert_eq!(
                totals.orphan_resident_bytes,
                orphans
                    .iter()
                    .map(|record| record.resident_bytes)
                    .sum::<u64>()
            );
            assert_eq!(
                totals.orphan_transient_bytes,
                orphans
                    .iter()
                    .map(|record| record.charged_transient())
                    .sum::<u64>()
            );
            for (key, record) in &ledger.resources {
                assert_eq!(
                    record.unprepared_owners,
                    record
                        .owners
                        .values()
                        .filter(|prepared| !**prepared)
                        .count()
                );
                for owner in record.owners.keys() {
                    assert!(ledger.owners[owner].contains(key));
                }
            }
            for (owner, keys) in &ledger.owners {
                for key in keys {
                    assert!(ledger.resources[key].owners.contains_key(owner));
                }
            }
        }
    }

    #[test]
    fn prepared_owner_abandonment_does_not_release_another_inflight_owners_scratch() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("ready".to_owned(), vec![cost("shared", 100, 50)], 150));
        assert!(ledger.reserve("loading".to_owned(), vec![cost("shared", 100, 50)], 150));
        ledger.mark_prepared("ready");
        assert!(ledger.abandon("ready").is_empty());
        assert_eq!(ledger.totals().transient_bytes, 50);
        assert_eq!(ledger.abandon("loading"), vec!["shared"]);
        assert_eq!(ledger.totals().orphan_transient_bytes, 50);
        assert!(!ledger.mark_prepared("loading"));
        assert_eq!(ledger.totals().peak_bytes(), Some(150));
        assert!(ledger.confirm_absent("shared"));
        assert_eq!(ledger.totals().peak_bytes(), Some(0));
    }

    #[test]
    fn scene_preflight_claim_transfers_to_a_placement_without_extra_charge() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve(
            "scene".to_owned(),
            vec![cost("geometry", 100, 50), cost("placement", 20, 10)],
            180
        ));
        assert!(ledger.reserve(
            "placement-owner".to_owned(),
            vec![cost("placement", 20, 10)],
            180
        ));
        let before = ledger.totals();
        assert!(ledger.detach_resource("scene", "placement"));
        assert_eq!(ledger.totals().resident_bytes, before.resident_bytes);
        assert_eq!(ledger.totals().transient_bytes, before.transient_bytes);
        assert_eq!(ledger.totals().orphan_resources, 0);
        assert_eq!(
            ledger.owner_resources("scene").unwrap(),
            &BTreeSet::from(["geometry".to_owned()])
        );
        assert!(!ledger.detach_resource("scene", "placement"));
        ledger.mark_prepared("placement-owner");
        assert_eq!(ledger.totals().transient_bytes, 50);
        assert_eq!(ledger.abandon("scene"), vec!["geometry"]);
        assert_eq!(ledger.totals().orphan_resources, 1);
        assert!(ledger.resource_has_owners("placement"));
    }

    #[test]
    fn detaching_a_final_inflight_claim_keeps_an_orphan_until_confirmed_disposal() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve(
            "owner".to_owned(),
            vec![cost("a", 100, 50), cost("b", 20, 10)],
            180
        ));
        assert!(ledger.detach_resource("owner", "a"));
        assert_eq!(ledger.totals().peak_bytes(), Some(180));
        assert_eq!(ledger.totals().orphan_bytes(), Some(150));
        assert!(ledger.confirm_absent("a"));
        assert_eq!(ledger.totals().peak_bytes(), Some(30));
        assert_eq!(ledger.totals().owners, 1);
        assert!(ledger.detach_resource("owner", "b"));
        assert_eq!(ledger.totals().owners, 0);
        assert_eq!(ledger.totals().orphan_bytes(), Some(30));
        assert!(!ledger.detach_resource("owner", "b"));
    }

    #[test]
    fn detaching_unprepared_claim_preserves_other_prepared_claim_and_releases_scratch() {
        let mut ledger = MemoryLedger::default();
        assert!(ledger.reserve("ready".to_owned(), vec![cost("shared", 100, 50)], 150));
        assert!(ledger.reserve("waiting".to_owned(), vec![cost("shared", 100, 50)], 150));
        ledger.mark_prepared("ready");
        assert!(ledger.detach_resource("waiting", "shared"));
        assert_eq!(ledger.totals().transient_bytes, 0);
        assert_eq!(ledger.totals().orphan_bytes(), Some(0));
        assert_eq!(ledger.totals().owners, 1);
        assert!(!ledger.confirm_absent("shared"));
    }
}
