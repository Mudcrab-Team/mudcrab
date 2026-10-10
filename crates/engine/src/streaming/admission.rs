//! Shared Scene(0) job admission. Counts bound outstanding scene jobs, not bytes,
//! dependency requests, decode memory, GPU residency, or frame duration.

use bevy::{prelude::*, world_serialization::WorldAsset};
use std::collections::{BTreeMap, HashSet};

/// Caller supplies a canonical converted path within the one active asset pack.
/// Identity fields prevent stale pack/chunk demand from reusing another record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SceneKey {
    pub canonical_path: String,
    pub build_identity: Option<String>,
    pub content_identity: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SceneDemand {
    pub key: SceneKey,
    pub subscriber: Entity,
    /// Adopt an already-live subscriber's handle; never issue another load for it.
    pub existing_handle: Option<Handle<WorldAsset>>,
    /// Lower values dispatch first. Root owns spatial/gameplay priority policy.
    pub priority: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SceneJobStatus {
    Loading,
    /// Root and recursive dependencies have both completed successfully.
    Ready,
    /// Root or recursive dependency failure is terminal for this demand lifetime.
    Failed,
    /// Only use after the owning loader confirms cancellation/completion.
    #[cfg_attr(not(test), allow(dead_code))]
    CanceledConfirmed,
}

enum Job {
    Queued,
    Dispatched {
        handle: Handle<WorldAsset>,
        status: SceneJobStatus,
    },
}

struct Record {
    subscribers: HashSet<Entity>,
    priority: u64,
    sequence: u64,
    job: Job,
}

#[derive(Resource, Default)]
pub(crate) struct SceneAdmission {
    records: BTreeMap<SceneKey, Record>,
    next_sequence: u64,
    dispatched_total: u64,
    completed_total: u64,
    failed_total: u64,
    canceled_total: u64,
    peak_active: usize,
    active_jobs: usize,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub(crate) struct AdmissionStats {
    pub active_jobs: usize,
    pub queued_jobs: usize,
    pub records: usize,
    pub subscribers: usize,
    pub orphan_active_jobs: usize,
    pub dispatched_total: u64,
    pub completed_total: u64,
    pub failed_total: u64,
    pub canceled_total: u64,
    pub peak_active: usize,
}

impl SceneAdmission {
    /// Call with the complete live full-cell AND LOD subscriber set each frame.
    /// Loading jobs survive subscriber removal until explicit terminal status.
    pub fn reconcile_demands(&mut self, demands: impl IntoIterator<Item = SceneDemand>) {
        for record in self.records.values_mut() {
            record.subscribers.clear();
            record.priority = u64::MAX;
        }
        for demand in demands {
            let sequence = self.next_sequence;
            let record = self.records.entry(demand.key).or_insert_with(|| {
                self.next_sequence = self.next_sequence.saturating_add(1);
                Record {
                    subscribers: HashSet::new(),
                    priority: u64::MAX,
                    sequence,
                    job: Job::Queued,
                }
            });
            record.subscribers.insert(demand.subscriber);
            record.priority = record.priority.min(demand.priority);
            if matches!(record.job, Job::Queued)
                && let Some(handle) = demand.existing_handle
            {
                // Until root reconciles load state, conservatively occupy a slot.
                record.job = Job::Dispatched {
                    handle,
                    status: SceneJobStatus::Loading,
                };
                self.active_jobs += 1;
            }
        }
        self.prune_unowned_terminal();
        self.peak_active = self.peak_active.max(self.active_jobs());
    }

    /// A snapshot of candidates; caller must dispatch each once before asking again.
    /// Zero keeps the unlimited immediate-loading policy. Existing adopted loads
    /// can already exceed a newly lowered cap; no further dispatch is then allowed.
    pub fn queued_keys(&self, max_scene_loads: usize) -> Vec<SceneKey> {
        let available = self.available_slots(max_scene_loads);
        let mut queued: Vec<_> = self
            .records
            .iter()
            .filter(|(_, record)| {
                matches!(record.job, Job::Queued) && !record.subscribers.is_empty()
            })
            .map(|(key, record)| (record.priority, record.sequence, key.clone()))
            .collect();
        queued.sort();
        queued
            .into_iter()
            .take(available)
            .map(|(_, _, key)| key)
            .collect()
    }

    /// Record exactly one AssetServer Scene(0) load dispatched by the owner.
    /// Returns false if a stale candidate was removed or already dispatched.
    pub fn dispatch(&mut self, key: &SceneKey, handle: Handle<WorldAsset>) -> bool {
        let Some(record) = self.records.get_mut(key) else {
            return false;
        };
        if !matches!(record.job, Job::Queued) || record.subscribers.is_empty() {
            return false;
        }
        record.job = Job::Dispatched {
            handle,
            status: SceneJobStatus::Loading,
        };
        self.dispatched_total = self.dispatched_total.saturating_add(1);
        self.active_jobs += 1;
        self.peak_active = self.peak_active.max(self.active_jobs());
        true
    }

    pub fn handle(&self, key: &SceneKey) -> Option<Handle<WorldAsset>> {
        match &self.records.get(key)?.job {
            Job::Queued => None,
            Job::Dispatched { handle, .. } => Some(handle.clone()),
        }
    }

    /// Root polls only outstanding jobs using exact IDs. Returned handles are
    /// temporary strong references; the registry itself retains the loading job.
    pub fn jobs(&self) -> Vec<(SceneKey, Handle<WorldAsset>)> {
        self.records
            .iter()
            .filter_map(|(key, record)| match &record.job {
                Job::Dispatched {
                    handle,
                    status: SceneJobStatus::Loading,
                } => Some((key.clone(), handle.clone())),
                _ => None,
            })
            .collect()
    }

    /// Terminal status cannot be changed back to Loading by new subscribers.
    pub fn set_status(&mut self, key: &SceneKey, status: SceneJobStatus) {
        let Some(record) = self.records.get_mut(key) else {
            return;
        };
        if let Job::Dispatched {
            status: current, ..
        } = &mut record.job
            && *current == SceneJobStatus::Loading
        {
            *current = status;
            if status != SceneJobStatus::Loading {
                self.active_jobs -= 1;
            }
            match status {
                SceneJobStatus::Ready => {
                    self.completed_total = self.completed_total.saturating_add(1)
                }
                SceneJobStatus::Failed => self.failed_total = self.failed_total.saturating_add(1),
                SceneJobStatus::CanceledConfirmed => {
                    self.canceled_total = self.canceled_total.saturating_add(1)
                }
                SceneJobStatus::Loading => {}
            }
        }
        // Only this record can have become terminal here. Avoid a full-ledger
        // retain scan for every polled job in an unlimited admission burst.
        if record.subscribers.is_empty()
            && !matches!(
                record.job,
                Job::Dispatched {
                    status: SceneJobStatus::Loading,
                    ..
                }
            )
        {
            self.records.remove(key);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn status(&self, key: &SceneKey) -> Option<SceneJobStatus> {
        match self.records.get(key)?.job {
            Job::Queued => None,
            Job::Dispatched { status, .. } => Some(status),
        }
    }

    /// Explicit retry only: remove a terminal failure when none of its previous
    /// owners remains live. Call before reconciliation; ordinary re-demand must
    /// never use this to restart a failed shared scene.
    pub fn retry_failed_if_unowned(
        &mut self,
        key: &SceneKey,
        live_subscribers: &HashSet<Entity>,
    ) -> bool {
        let removable = self.records.get(key).is_some_and(|record| {
            matches!(
                record.job,
                Job::Dispatched {
                    status: SceneJobStatus::Failed,
                    ..
                }
            ) && record.subscribers.is_disjoint(live_subscribers)
        });
        if removable {
            self.records.remove(key);
        }
        removable
    }

    pub fn active_jobs(&self) -> usize {
        self.active_jobs
    }

    pub fn queued_jobs(&self) -> usize {
        self.records
            .values()
            .filter(|record| matches!(record.job, Job::Queued))
            .count()
    }

    pub fn available_slots(&self, max_scene_loads: usize) -> usize {
        if max_scene_loads == 0 {
            usize::MAX
        } else {
            max_scene_loads.saturating_sub(self.active_jobs())
        }
    }

    pub fn stats(&self) -> AdmissionStats {
        AdmissionStats {
            active_jobs: self.active_jobs(),
            queued_jobs: self.queued_jobs(),
            records: self.records.len(),
            subscribers: self
                .records
                .values()
                .map(|record| record.subscribers.len())
                .sum(),
            orphan_active_jobs: self
                .records
                .values()
                .filter(|record| {
                    record.subscribers.is_empty()
                        && matches!(
                            record.job,
                            Job::Dispatched {
                                status: SceneJobStatus::Loading,
                                ..
                            }
                        )
                })
                .count(),
            dispatched_total: self.dispatched_total,
            completed_total: self.completed_total,
            failed_total: self.failed_total,
            canceled_total: self.canceled_total,
            peak_active: self.peak_active,
        }
    }

    fn prune_unowned_terminal(&mut self) {
        self.records.retain(|_, record| {
            !record.subscribers.is_empty()
                || matches!(
                    record.job,
                    Job::Dispatched {
                        status: SceneJobStatus::Loading,
                        ..
                    }
                )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> SceneKey {
        SceneKey {
            canonical_path: path.into(),
            build_identity: Some("pack-a".into()),
            content_identity: None,
        }
    }
    fn demand(path: &str, subscriber: u32) -> SceneDemand {
        SceneDemand {
            key: key(path),
            subscriber: Entity::from_raw_u32(subscriber).unwrap(),
            existing_handle: None,
            priority: u64::from(subscriber),
        }
    }
    fn dispatch(admission: &mut SceneAdmission, path: &str) {
        assert!(admission.dispatch(&key(path), Handle::default()));
    }

    #[test]
    fn shared_fanout_consumes_one_slot_and_zero_is_unlimited() {
        let mut admission = SceneAdmission::default();
        admission.reconcile_demands([demand("a", 1), demand("a", 2), demand("b", 3)]);
        assert_eq!(admission.queued_keys(1), vec![key("a")]);
        dispatch(&mut admission, "a");
        assert_eq!(admission.active_jobs(), 1);
        let stats = admission.stats();
        assert_eq!(stats.subscribers, 3);
        assert_eq!(stats.records, 2);
        assert_eq!(stats.dispatched_total, 1);
        assert_eq!(stats.peak_active, 1);
        assert!(admission.queued_keys(1).is_empty());
        assert_eq!(admission.queued_keys(0), vec![key("b")]);
    }

    #[test]
    fn orphaned_loading_job_keeps_slot_until_completion() {
        let mut admission = SceneAdmission::default();
        admission.reconcile_demands([demand("a", 1)]);
        dispatch(&mut admission, "a");
        admission.reconcile_demands([demand("b", 2)]);
        assert!(admission.handle(&key("a")).is_some());
        assert!(admission.queued_keys(1).is_empty());
        assert_eq!(admission.stats().orphan_active_jobs, 1);
        admission.set_status(&key("a"), SceneJobStatus::Ready);
        assert_eq!(admission.stats().completed_total, 1);
        assert!(admission.handle(&key("a")).is_none());
        assert_eq!(admission.queued_keys(1), vec![key("b")]);
    }

    #[test]
    fn failure_persists_with_subscribers_and_reload_requires_new_lifetime() {
        let mut admission = SceneAdmission::default();
        admission.reconcile_demands([demand("a", 1)]);
        dispatch(&mut admission, "a");
        admission.set_status(&key("a"), SceneJobStatus::Failed);
        admission.reconcile_demands([demand("a", 2)]);
        admission.set_status(&key("a"), SceneJobStatus::Loading);
        assert_eq!(admission.status(&key("a")), Some(SceneJobStatus::Failed));
        assert_eq!(admission.active_jobs(), 0);
        assert!(admission.queued_keys(1).is_empty());
        admission.reconcile_demands([]);
        admission.reconcile_demands([demand("a", 3)]);
        assert_eq!(admission.queued_keys(1), vec![key("a")]);
    }

    #[test]
    fn fast_return_adopts_handle_without_dispatch_and_identity_isolated() {
        let mut admission = SceneAdmission::default();
        let mut live = demand("a", 1);
        live.existing_handle = Some(Handle::default());
        admission.reconcile_demands([live]);
        admission.set_status(&key("a"), SceneJobStatus::Ready);
        admission.reconcile_demands([demand("a", 2)]);
        assert!(admission.handle(&key("a")).is_some());
        assert!(admission.queued_keys(1).is_empty());
        let mut other = demand("a", 3);
        other.key.content_identity = Some("different-chunk".into());
        admission.reconcile_demands([demand("a", 2), other.clone()]);
        assert_eq!(admission.queued_keys(1), vec![other.key]);
    }

    #[test]
    fn confirmed_cancellation_releases_orphan_slot_and_queued_orphans_disappear() {
        let mut admission = SceneAdmission::default();
        admission.reconcile_demands([demand("a", 1), demand("b", 2)]);
        dispatch(&mut admission, "a");
        admission.reconcile_demands([]);
        assert_eq!(admission.queued_jobs(), 0);
        assert_eq!(admission.active_jobs(), 1);
        admission.set_status(&key("a"), SceneJobStatus::CanceledConfirmed);
        assert_eq!(admission.active_jobs(), 0);
        assert_eq!(admission.stats().canceled_total, 1);
        assert!(admission.jobs().is_empty());
    }

    #[test]
    fn explicit_retry_requires_failure_and_no_live_previous_owner() {
        let mut admission = SceneAdmission::default();
        admission.reconcile_demands([demand("a", 1), demand("a", 2)]);
        dispatch(&mut admission, "a");
        assert!(!admission.retry_failed_if_unowned(&key("a"), &HashSet::new()));
        assert_eq!(admission.active_jobs(), 1);
        admission.set_status(&key("a"), SceneJobStatus::Failed);
        let owner = Entity::from_raw_u32(2).unwrap();
        assert!(!admission.retry_failed_if_unowned(&key("a"), &HashSet::from([owner])));
        assert!(admission.handle(&key("a")).is_some());
        let new_owner = Entity::from_raw_u32(3).unwrap();
        assert!(admission.retry_failed_if_unowned(&key("a"), &HashSet::from([new_owner])));
        assert!(admission.handle(&key("a")).is_none());
        admission.reconcile_demands([demand("a", 3)]);
        assert_eq!(admission.queued_keys(1), vec![key("a")]);
        assert_eq!(admission.stats().failed_total, 1);
    }

    #[test]
    fn active_counter_survives_reconcile_and_duplicate_terminal_updates() {
        let mut admission = SceneAdmission::default();
        let mut adopted = demand("a", 1);
        adopted.existing_handle = Some(Handle::default());
        admission.reconcile_demands([adopted.clone(), demand("b", 2)]);
        admission.reconcile_demands([adopted.clone(), demand("b", 2)]);
        assert_eq!(admission.active_jobs(), 1);
        assert_eq!(admission.available_slots(2), 1);
        dispatch(&mut admission, "b");
        assert_eq!(admission.active_jobs(), 2);
        admission.set_status(&key("a"), SceneJobStatus::Loading);
        assert_eq!(admission.active_jobs(), 2);
        admission.set_status(&key("a"), SceneJobStatus::Ready);
        admission.set_status(&key("a"), SceneJobStatus::Ready);
        admission.set_status(&key("a"), SceneJobStatus::Failed);
        assert_eq!(admission.active_jobs(), 1);
        assert_eq!(admission.stats().completed_total, 1);
        assert_eq!(admission.stats().failed_total, 0);
        admission.reconcile_demands([adopted]);
        assert_eq!(admission.active_jobs(), 1); // b remains an orphan loading job.
        admission.set_status(&key("b"), SceneJobStatus::Failed);
        admission.set_status(&key("b"), SceneJobStatus::Failed);
        assert_eq!(admission.active_jobs(), 0);
        assert_eq!(admission.stats().peak_active, 2);
        assert_eq!(admission.stats().failed_total, 1);
    }
}
