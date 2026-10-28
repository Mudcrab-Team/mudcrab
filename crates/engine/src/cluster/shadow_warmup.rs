//! View-independent shadow pipeline planning and event-driven compilation proof.

use bevy::render::render_resource::CachedRenderPipelineId;

/// Exact inputs to the set of shadow pipeline specializations a scene needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ShadowPlanStamp {
    pub(super) gpu: u64,
    pub(super) generation: u64,
    pub(super) layout_epoch: u64,
    pub(super) batch_material_version: u64,
    pub(super) source_layout: u64,
    pub(super) batch_count: usize,
    pub(super) layouts_ready: bool,
}

#[derive(Default)]
pub(super) struct ShadowWarmup {
    stamp: Option<ShadowPlanStamp>,
    required: Vec<CachedRenderPipelineId>,
    complete: bool,
    pipeline_revision: Option<u64>,
    ready: bool,
}

impl ShadowWarmup {
    /// Request both light projection families without depending on visible lights.
    /// Missing dependencies leave the plan incomplete so the next call retries.
    pub(super) fn prepare(
        &mut self,
        stamp: ShadowPlanStamp,
        mut request: impl FnMut(bool, &mut Vec<CachedRenderPipelineId>) -> bool,
    ) {
        if self.stamp == Some(stamp) && self.complete {
            return;
        }
        let mut required = Vec::new();
        let mut complete = true;
        for unclipped in [false, true] {
            // Do not short-circuit: both families must be requested even when
            // one is missing a material or mesh dependency.
            complete &= request(unclipped, &mut required);
        }
        self.publish(stamp, required, complete);
    }

    pub(super) fn publish(
        &mut self,
        stamp: ShadowPlanStamp,
        mut required: Vec<CachedRenderPipelineId>,
        complete: bool,
    ) {
        required.sort_unstable_by_key(CachedRenderPipelineId::id);
        required.dedup();
        self.stamp = Some(stamp);
        self.required = required;
        self.complete = complete;
        // A new plan needs a fresh proof even if the pipeline cache has not
        // changed since the old plan was checked.
        self.pipeline_revision = None;
        self.ready = false;
    }

    /// Read pipeline states only after a plan or availability revision changes.
    pub(super) fn refresh(
        &mut self,
        pipeline_revision: u64,
        mut ready: impl FnMut(CachedRenderPipelineId) -> bool,
    ) {
        if self.pipeline_revision == Some(pipeline_revision) {
            return;
        }
        self.pipeline_revision = Some(pipeline_revision);
        self.ready = self.complete && self.required.iter().copied().all(&mut ready);
    }

    pub(super) fn is_ready(&self) -> bool {
        self.ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> ShadowPlanStamp {
        ShadowPlanStamp {
            gpu: 1,
            generation: 2,
            layout_epoch: 3,
            batch_material_version: 4,
            source_layout: 5,
            batch_count: 6,
            layouts_ready: true,
        }
    }

    #[test]
    fn shadow_warmup_requests_both_families_without_views() {
        let mut warmup = ShadowWarmup::default();
        let mut requested = Vec::new();
        warmup.prepare(stamp(), |unclipped, required| {
            requested.push(unclipped);
            let first = 2 * usize::from(unclipped);
            required.push(CachedRenderPipelineId::new(first));
            required.push(CachedRenderPipelineId::new(first + 1));
            true
        });
        let mut checked = Vec::new();
        warmup.refresh(1, |id| {
            checked.push(id.id());
            true
        });
        assert_eq!(requested, [false, true]);
        assert_eq!(checked, [0, 1, 2, 3]);
        assert!(warmup.is_ready());
    }

    #[test]
    fn shadow_warmup_requires_every_pipeline_compiled() {
        let mut warmup = ShadowWarmup::default();
        warmup.publish(
            stamp(),
            vec![
                CachedRenderPipelineId::new(1),
                CachedRenderPipelineId::new(2),
            ],
            true,
        );
        warmup.refresh(1, |id| id.id() == 1);
        assert!(!warmup.is_ready());
        warmup.refresh(2, |_| true);
        assert!(warmup.is_ready());
    }

    #[test]
    fn shadow_warmup_new_plan_invalidates_same_pipeline_epoch() {
        let mut warmup = ShadowWarmup::default();
        warmup.publish(stamp(), vec![CachedRenderPipelineId::new(1)], true);
        warmup.refresh(7, |_| true);
        assert!(warmup.is_ready());
        let mut next = stamp();
        next.generation += 1;
        warmup.publish(next, vec![CachedRenderPipelineId::new(2)], true);
        assert!(!warmup.is_ready());
        let mut checked = Vec::new();
        warmup.refresh(7, |id| {
            checked.push(id.id());
            false
        });
        assert_eq!(checked, [2]);
        assert!(!warmup.is_ready());
    }

    #[test]
    fn shadow_warmup_shader_withdrawal_revokes_readiness() {
        let mut warmup = ShadowWarmup::default();
        warmup.publish(stamp(), vec![CachedRenderPipelineId::new(1)], true);
        warmup.refresh(1, |_| true);
        assert!(warmup.is_ready());
        warmup.refresh(2, |_| false);
        assert!(!warmup.is_ready());
        warmup.refresh(3, |_| true);
        assert!(warmup.is_ready());
    }

    #[test]
    fn shadow_warmup_unchanged_epoch_and_plan_do_no_work() {
        let mut warmup = ShadowWarmup::default();
        warmup.publish(stamp(), vec![CachedRenderPipelineId::new(1)], true);
        warmup.refresh(1, |_| true);
        warmup.prepare(stamp(), |_, _| panic!("settled plan rebuilt"));
        warmup.refresh(1, |_| panic!("settled pipeline readiness scanned"));
        assert!(warmup.is_ready());
    }

    #[test]
    fn shadow_warmup_incomplete_plan_retries_both_families() {
        let mut warmup = ShadowWarmup::default();
        let mut requested = Vec::new();
        warmup.prepare(stamp(), |unclipped, required| {
            requested.push(unclipped);
            required.push(CachedRenderPipelineId::new(usize::from(unclipped)));
            unclipped
        });
        warmup.refresh(1, |_| true);
        assert_eq!(requested, [false, true]);
        assert!(!warmup.is_ready());
        requested.clear();
        warmup.prepare(stamp(), |unclipped, required| {
            requested.push(unclipped);
            required.push(CachedRenderPipelineId::new(usize::from(unclipped)));
            true
        });
        warmup.refresh(1, |_| true);
        assert_eq!(requested, [false, true]);
        assert!(warmup.is_ready());
    }

    #[test]
    fn shadow_warmup_exact_stamp_rebuilds_for_each_input() {
        let initial = stamp();
        let mut changes = [initial; 7];
        changes[0].gpu += 1;
        changes[1].generation += 1;
        changes[2].layout_epoch += 1;
        changes[3].batch_material_version += 1;
        changes[4].source_layout += 1;
        changes[5].batch_count += 1;
        changes[6].layouts_ready = false;
        for changed in changes {
            let mut warmup = ShadowWarmup::default();
            warmup.publish(initial, vec![CachedRenderPipelineId::new(1)], true);
            warmup.refresh(1, |_| true);
            let mut requested = Vec::new();
            warmup.prepare(changed, |unclipped, required| {
                requested.push(unclipped);
                required.push(CachedRenderPipelineId::new(2));
                true
            });
            assert_eq!(requested, [false, true], "stamp change: {changed:?}");
            assert!(!warmup.is_ready());
        }
    }
}
