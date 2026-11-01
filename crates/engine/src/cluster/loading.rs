//! Cached loading facts over exact changed render-world inputs.
//!
//! The renderer publishes only when needed batch/view inputs change. Package,
//! layout and generation scope every snapshot; consumers own transition policy.

use super::reader::CertifiedPackage;
use bevy::prelude::*;
use std::{collections::HashMap, sync::Arc};

/// Counts for all applicable consumers of one exterior worldspace. Needed batches
/// count batch/view pairs. A missing GPU view remains a missing consumer, even if
/// none of its batches can be enumerated yet. At least one camera is required.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClusterWorldReadiness {
    pub applicable_cameras: usize,
    pub missing_views: usize,
    pub needed_batches: usize,
    pub pending_materials: usize,
    pub pending_templates: usize,
    pub pending_compute: usize,
    pub pending_pipelines: usize,
    pub failed_batches: usize,
}

impl ClusterWorldReadiness {
    #[cfg(test)]
    fn missing(self) -> usize {
        usize::from(self.applicable_cameras == 0)
            .saturating_add(self.missing_views)
            .saturating_add(self.pending_materials)
            .saturating_add(self.pending_templates)
            .saturating_add(self.pending_compute)
            .saturating_add(self.pending_pipelines)
            .saturating_add(self.failed_batches)
    }
}

/// Main-world loading facts, scoped to the current certified package and layout.
/// Publishing is not authority to reveal stock or to hide loading during play.
#[derive(Resource)]
pub(crate) struct ClusterLoadingGate {
    package: Option<Arc<CertifiedPackage>>,
    layout: u64,
    generation: u64,
    snapshot_generation: Option<u64>,
    render_frame: Option<u64>,
    worlds: HashMap<u32, ClusterWorldReadiness>,
}

impl Default for ClusterLoadingGate {
    fn default() -> Self {
        Self {
            package: None,
            layout: 0,
            generation: 1,
            snapshot_generation: None,
            render_frame: None,
            worlds: HashMap::new(),
        }
    }
}

impl ClusterLoadingGate {
    #[cfg(test)]
    pub(crate) fn world_ready(&self, world: u32) -> bool {
        self.owned_not_drawable(world) == 0
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn withdraw(&mut self) {
        self.snapshot_generation = None;
        self.worlds.clear();
    }

    /// The producer must check package/layout against the current main-world
    /// scope before publishing. `generation` belongs to the evaluated render
    /// snapshot, never the generation read later when extracting its result.
    pub(crate) fn publish(
        &mut self,
        package: &Arc<CertifiedPackage>,
        layout: u64,
        generation: u64,
        render_frame: u64,
        worlds: HashMap<u32, ClusterWorldReadiness>,
    ) -> bool {
        if generation != self.generation
            || self
                .render_frame
                .is_some_and(|previous| render_frame < previous)
        {
            return false;
        }
        if self.layout != layout
            || self
                .package
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, package))
        {
            self.withdraw();
            self.package = Some(package.clone());
            self.layout = layout;
        }
        self.worlds = worlds;
        self.snapshot_generation = Some(generation);
        self.render_frame = Some(render_frame);
        true
    }

    /// Missing dependency count, with one sentinel for absent/stale observations.
    /// Counts can overlap; this is not a distinct-object count.
    #[cfg(test)]
    pub(crate) fn owned_not_drawable(&self, world: u32) -> usize {
        if self.snapshot_generation != Some(self.generation) {
            return 1;
        }
        self.worlds.get(&world).map_or(1, |world| world.missing())
    }
}

pub(crate) fn install(app: &mut App) {
    app.init_resource::<ClusterLoadingGate>();
}
