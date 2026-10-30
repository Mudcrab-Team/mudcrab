//! Allocation-local validity of the eight-stage cluster selection result.
//!
//! Writers publish revisions; checking an unchanged view never walks its instances,
//! materials, masks, or source-order words. Uniform equality is checked separately
//! against the exact bytes already uploaded to this view's parameter allocation.

/// One process-wide namespace for both cull results and raster-only dependencies.
pub(super) fn next_revision() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |revision| revision.checked_add(1),
    )
    .expect("cluster output revision exhausted")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CullInputs {
    pub gpu: u64,
    pub layout: u64,
    pub materials: u64,
    pub selection: u64,
    pub plan: u64,
    pub readiness: u64,
    pub pipelines: u64,
    pub origin: [u32; 3],
    /// Full view inputs also invalidate when an explicit clip_from_world makes
    /// one changed projection/pose word irrelevant to the derived uniform.
    pub world_from_view: [u32; 16],
    pub clip_from_view: [u32; 16],
    pub clip_from_world: Option<[u32; 16]>,
    pub viewport: [u32; 4],
    pub invert_culling: bool,
}

/// Owned by one ViewGpu and destroyed with its output allocations.
#[derive(Default)]
pub(super) struct CullReuse {
    encoded: Option<CullInputs>,
    revision: u64,
}

impl CullReuse {
    /// An input change withdraws the old result before any GPU work is encoded.
    pub fn needs_recompute(&mut self, inputs: CullInputs, uniform_changed: bool) -> bool {
        if !uniform_changed && self.encoded == Some(inputs) {
            return false;
        }
        self.invalidate();
        true
    }

    /// Publish only after all eight stages were encoded successfully.
    pub fn encoded(&mut self, inputs: CullInputs) {
        // A recreated ViewGpu must never alias the revision published by its
        // predecessor, including when its retained entity and inputs match.
        self.revision = next_revision();
        self.encoded = Some(inputs);
    }

    pub fn invalidate(&mut self) {
        self.encoded = None;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
#[path = "cull_reuse_tests.rs"]
mod tests;
