//! Optional observation of API draw calls issued by Bevy's original mesh command.
//!
//! Atomic counters add submission overhead. Enable only for a separately labelled count capture.
use crate::profiling::ProfilingState;
use bevy::{
    core_pipeline::{
        core_3d::{AlphaMask3d, Opaque3d, Transparent3d},
        deferred::{AlphaMask3dDeferred, Opaque3dDeferred},
        prepass::{AlphaMask3dPrepass, Opaque3dPrepass},
    },
    ecs::{
        query::ROQueryItem,
        system::{ReadOnlySystemParam, SystemParamItem, lifetimeless::SRes},
    },
    pbr::{DrawDepthOnlyPrepass, DrawMaterial, DrawMesh, DrawPrepass, Shadow, Transmissive3d},
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        mesh::RenderMeshBufferInfo,
        render_phase::{
            DrawFunctions, PhaseItem, PhaseItemExtraIndex, RenderCommand, RenderCommandResult,
            RenderCommandState, TrackedRenderPass,
        },
    },
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

pub(crate) const MAX_DRAW_FRAME_SAMPLES: usize = 20_000;

const PHASE_NAMES: [&str; 9] = [
    "opaque",
    "alpha_mask",
    "transparent",
    "transmissive",
    "opaque_prepass",
    "alpha_mask_prepass",
    "opaque_deferred",
    "alpha_mask_deferred",
    "shadow",
];
const EXPECTED_VARIANTS: [u64; 9] = [1, 1, 1, 1, 2, 1, 1, 1, 2];

pub struct IndirectDrawMetricsPlugin;

impl Plugin for IndirectDrawMetricsPlugin {
    fn build(&self, app: &mut App) {
        let bridge = DrawCountBridge::default();
        app.insert_resource(bridge.clone())
            .add_systems(Update, sync_draw_counts);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.insert_resource(bridge).add_systems(
                Render,
                (
                    count_render_schedule.in_set(RenderSystems::Prepare),
                    publish_draw_counts.in_set(RenderSystems::Cleanup),
                ),
            );
        }
    }

    fn finish(&self, app: &mut App) {
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            install_observed_draws(render_app.world_mut());
        }
    }
}

macro_rules! draw_counters {
    ($($field:ident),+ $(,)?) => {
        #[derive(Default)]
        struct AtomicDrawCounts { $($field: AtomicU64),+ }

        #[derive(Debug, Clone, Default, Serialize)]
        pub struct DrawCounts { $(pub $field: u64),+ }

        impl DrawCounts {
            fn since(&self, previous: &Self) -> Self {
                Self { $($field: self.$field.saturating_sub(previous.$field)),+ }
            }
        }

        impl AtomicDrawCounts {
            fn snapshot(&self) -> DrawCounts {
                DrawCounts { $($field: self.$field.load(Ordering::Relaxed)),+ }
            }
        }
    };
}

draw_counters!(
    leaf_mesh_commands_reached,
    leaf_mesh_commands_skipped,
    leaf_mesh_command_failures,
    issued_api_calls,
    direct_indexed_api_calls,
    direct_non_indexed_api_calls,
    fixed_indirect_api_calls,
    fixed_indexed_argument_slots,
    fixed_non_indexed_argument_slots,
    empty_fixed_indirect_ranges,
    dynamic_indirect_api_calls,
    dynamic_argument_slot_upper_bound,
    unclassified_successes,
);

struct DrawCountState {
    started: Instant,
    render_schedule_frames: AtomicU64,
    counts: [AtomicDrawCounts; PHASE_NAMES.len()],
    registry_present: [AtomicU64; PHASE_NAMES.len()],
    registered_variants: [AtomicU64; PHASE_NAMES.len()],
    wrapped_variants: [AtomicU64; PHASE_NAMES.len()],
}

impl Default for DrawCountState {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            render_schedule_frames: AtomicU64::new(0),
            counts: std::array::from_fn(|_| AtomicDrawCounts::default()),
            registry_present: std::array::from_fn(|_| AtomicU64::new(0)),
            registered_variants: std::array::from_fn(|_| AtomicU64::new(0)),
            wrapped_variants: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

#[derive(Resource, Clone, Default)]
struct DrawCountBridge(Arc<DrawCountState>, Arc<Mutex<PublishedDrawCounts>>);

#[derive(Default)]
struct PublishedDrawCounts {
    totals: Option<DrawSubmissionReport>,
    frames: Vec<DrawSubmissionFrame>,
    dropped_frame_samples: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DrawSubmissionFrame {
    pub render_frame: u64,
    pub elapsed_seconds: f64,
    pub phases: BTreeMap<&'static str, DrawCounts>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseDrawReport {
    pub phase: &'static str,
    pub registry_present: bool,
    pub registered_draw_variants: u64,
    pub wrapped_draw_variants: u64,
    pub expected_draw_variants: u64,
    pub unwrapped_draw_variants: u64,
    pub coverage_complete: bool,
    pub counts: DrawCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct DrawSubmissionReport {
    pub instrumentation: &'static str,
    pub scope: &'static str,
    pub render_schedule_frames: u64,
    pub coverage_complete: bool,
    pub dropped_frame_samples: u64,
    pub phases: Vec<PhaseDrawReport>,
}

impl DrawCountBridge {
    fn snapshot(&self) -> DrawSubmissionReport {
        let phases: Vec<_> = PHASE_NAMES
            .iter()
            .enumerate()
            .map(|(index, phase)| {
                let present = self.0.registry_present[index].load(Ordering::Relaxed) != 0;
                let registered = self.0.registered_variants[index].load(Ordering::Relaxed);
                let wrapped = self.0.wrapped_variants[index].load(Ordering::Relaxed);
                let unwrapped = registered.saturating_sub(wrapped);
                PhaseDrawReport {
                    phase,
                    registry_present: present,
                    registered_draw_variants: registered,
                    wrapped_draw_variants: wrapped,
                    expected_draw_variants: EXPECTED_VARIANTS[index],
                    unwrapped_draw_variants: unwrapped,
                    coverage_complete: !present
                        || (wrapped == EXPECTED_VARIANTS[index] && unwrapped == 0),
                    counts: self.0.counts[index].snapshot(),
                }
            })
            .collect();
        DrawSubmissionReport {
            instrumentation: "atomic counters after the original Bevy DrawMesh command",
            scope: "through the last completed render schedule, including warmup; shared StandardMaterial and ExtendedMaterial draw paths; excludes non-mesh render nodes",
            render_schedule_frames: self.0.render_schedule_frames.load(Ordering::Relaxed),
            coverage_complete: phases.iter().all(|phase| phase.coverage_complete),
            dropped_frame_samples: 0,
            phases,
        }
    }
}

fn sync_draw_counts(bridge: Res<DrawCountBridge>, mut profiler: ResMut<ProfilingState>) {
    let snapshot = bridge.1.lock().ok().and_then(|mut published| {
        published
            .totals
            .clone()
            .map(|report| (report, std::mem::take(&mut published.frames)))
    });
    if let Some((report, frames)) = snapshot {
        profiler.set_draw_submission(report, frames);
    }
}

fn publish_draw_counts(bridge: Res<DrawCountBridge>) {
    let mut report = bridge.snapshot();
    if let Ok(mut published) = bridge.1.lock() {
        let phases = report
            .phases
            .iter()
            .enumerate()
            .map(|(index, phase)| {
                let previous = published
                    .totals
                    .as_ref()
                    .map(|last| &last.phases[index].counts)
                    .cloned()
                    .unwrap_or_default();
                (phase.phase, phase.counts.since(&previous))
            })
            .collect();
        if published.frames.len() < MAX_DRAW_FRAME_SAMPLES {
            published.frames.push(DrawSubmissionFrame {
                render_frame: report.render_schedule_frames,
                elapsed_seconds: bridge.0.started.elapsed().as_secs_f64(),
                phases,
            });
        } else {
            published.dropped_frame_samples = published.dropped_frame_samples.saturating_add(1);
        }
        report.dropped_frame_samples = published.dropped_frame_samples;
        published.totals = Some(report);
    }
}

fn count_render_schedule(bridge: Res<DrawCountBridge>) {
    bridge
        .0
        .render_schedule_frames
        .fetch_add(1, Ordering::Relaxed);
}

trait ObservedPhase: PhaseItem {
    const INDEX: usize;
}
macro_rules! observed_phases {
    ($($phase:ty => $index:expr),+ $(,)?) => { $(
        impl ObservedPhase for $phase { const INDEX: usize = $index; }
    )+ };
}
observed_phases!(
    Opaque3d => 0, AlphaMask3d => 1, Transparent3d => 2, Transmissive3d => 3,
    Opaque3dPrepass => 4, AlphaMask3dPrepass => 5, Opaque3dDeferred => 6,
    AlphaMask3dDeferred => 7, Shadow => 8,
);

// Preserve every original tuple member and change only the final mesh command.
trait WithObservedMesh {
    type Observed;
}
impl<A, B, C, D, E> WithObservedMesh for (A, B, C, D, E, DrawMesh) {
    type Observed = (A, B, C, D, E, CountedDrawMesh);
}

fn replace_draw<P, C>(world: &mut World)
where
    P: ObservedPhase,
    C: RenderCommand<P> + WithObservedMesh + 'static,
    C::Observed: RenderCommand<P> + Send + Sync + 'static,
    <C::Observed as RenderCommand<P>>::Param: ReadOnlySystemParam,
{
    let Some(functions) = world.get_resource::<DrawFunctions<P>>() else {
        return;
    };
    let Some(id) = functions.read().get_id::<C>() else {
        return;
    };
    let replacement = RenderCommandState::<P, C::Observed>::new(world);
    world.resource::<DrawFunctions<P>>().write().draw_functions[id.0 as usize] =
        Box::new(replacement);
    world.resource::<DrawCountBridge>().0.wrapped_variants[P::INDEX]
        .fetch_add(1, Ordering::Relaxed);
}

fn record_registry<P: ObservedPhase>(world: &World) {
    if let Some(functions) = world.get_resource::<DrawFunctions<P>>() {
        let bridge = world.resource::<DrawCountBridge>();
        bridge.0.registry_present[P::INDEX].store(1, Ordering::Relaxed);
        bridge.0.registered_variants[P::INDEX].store(
            functions.read().draw_functions.len() as u64,
            Ordering::Relaxed,
        );
    }
}

fn install_observed_draws(world: &mut World) {
    macro_rules! install {
        ($phase:ty, $($command:ty),+) => {
            record_registry::<$phase>(world);
            $(replace_draw::<$phase, $command>(world);)+
        };
    }
    install!(Opaque3d, DrawMaterial);
    install!(AlphaMask3d, DrawMaterial);
    install!(Transparent3d, DrawMaterial);
    install!(Transmissive3d, DrawMaterial);
    install!(Opaque3dPrepass, DrawPrepass, DrawDepthOnlyPrepass);
    install!(AlphaMask3dPrepass, DrawPrepass);
    install!(Opaque3dDeferred, DrawPrepass);
    install!(AlphaMask3dDeferred, DrawPrepass);
    install!(Shadow, DrawPrepass, DrawDepthOnlyPrepass);
}

struct CountedDrawMesh;
impl<P: ObservedPhase> RenderCommand<P> for CountedDrawMesh {
    type Param = (<DrawMesh as RenderCommand<P>>::Param, SRes<DrawCountBridge>);
    type ViewQuery = <DrawMesh as RenderCommand<P>>::ViewQuery;
    type ItemQuery = <DrawMesh as RenderCommand<P>>::ItemQuery;

    fn render<'w>(
        item: &P,
        view: ROQueryItem<'w, '_, Self::ViewQuery>,
        entity: Option<ROQueryItem<'w, '_, Self::ItemQuery>>,
        (original, bridge): SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let indexed = original
            .1
            .mesh_asset_id(item.main_entity())
            .and_then(|id| original.0.get(id))
            .map(|mesh| matches!(mesh.buffer_info, RenderMeshBufferInfo::Indexed { .. }));
        let result = <DrawMesh as RenderCommand<P>>::render(item, view, entity, original, pass);
        let counts = &bridge.0.counts[P::INDEX];
        counts
            .leaf_mesh_commands_reached
            .fetch_add(1, Ordering::Relaxed);
        match &result {
            RenderCommandResult::Success => record_issued_draw(counts, indexed, item.extra_index()),
            RenderCommandResult::Skip => {
                counts
                    .leaf_mesh_commands_skipped
                    .fetch_add(1, Ordering::Relaxed);
            }
            RenderCommandResult::Failure(_) => {
                counts
                    .leaf_mesh_command_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        result
    }
}

fn record_issued_draw(
    counts: &AtomicDrawCounts,
    indexed: Option<bool>,
    extra: PhaseItemExtraIndex,
) {
    counts.issued_api_calls.fetch_add(1, Ordering::Relaxed);
    let Some(indexed) = indexed else {
        counts
            .unclassified_successes
            .fetch_add(1, Ordering::Relaxed);
        return;
    };
    match extra {
        PhaseItemExtraIndex::None | PhaseItemExtraIndex::DynamicOffset(_) => {
            let direct = if indexed {
                &counts.direct_indexed_api_calls
            } else {
                &counts.direct_non_indexed_api_calls
            };
            direct.fetch_add(1, Ordering::Relaxed);
        }
        PhaseItemExtraIndex::IndirectParametersIndex {
            range,
            batch_set_index,
        } => {
            let slots = range.end.saturating_sub(range.start) as u64;
            if batch_set_index.is_some() {
                counts
                    .dynamic_indirect_api_calls
                    .fetch_add(1, Ordering::Relaxed);
                counts
                    .dynamic_argument_slot_upper_bound
                    .fetch_add(slots, Ordering::Relaxed);
            } else {
                counts
                    .fixed_indirect_api_calls
                    .fetch_add(1, Ordering::Relaxed);
                let fixed = if indexed {
                    &counts.fixed_indexed_argument_slots
                } else {
                    &counts.fixed_non_indexed_argument_slots
                };
                fixed.fetch_add(slots, Ordering::Relaxed);
                if slots == 0 {
                    counts
                        .empty_fixed_indirect_ranges
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        ecs::system::RunSystemOnce,
        pbr::{
            SetMaterialBindGroup, SetMeshBindGroup, SetMeshViewBindGroup,
            SetMeshViewBindingArrayBindGroup,
        },
        render::render_phase::{Draw, DrawError, SetItemPipeline},
    };
    use std::any::TypeId;

    struct DummyDraw;
    impl Draw<Opaque3d> for DummyDraw {
        fn draw<'w>(
            &mut self,
            _world: &'w World,
            _pass: &mut TrackedRenderPass<'w>,
            _view: Entity,
            _item: &Opaque3d,
        ) -> Result<(), DrawError> {
            Ok(())
        }
    }

    #[test]
    fn registry_substitution_preserves_original_id_and_other_draw_functions() {
        let mut world = World::new();
        world.init_resource::<DrawCountBridge>();
        let functions = DrawFunctions::<Opaque3d>::default();
        let (unrelated_id, original_id, unrelated_ptr, original_ptr) = {
            let mut functions = functions.write();
            let unrelated = functions.add(DummyDraw);
            let original = functions.add_with::<DrawMaterial, _>(DummyDraw);
            let pointer = |index: usize| {
                functions.draw_functions[index].as_ref() as *const dyn Draw<Opaque3d> as *const ()
            };
            (
                unrelated,
                original,
                pointer(unrelated.0 as usize),
                pointer(original.0 as usize),
            )
        };
        world.insert_resource(functions);
        replace_draw::<Opaque3d, DrawMaterial>(&mut world);
        let functions = world.resource::<DrawFunctions<Opaque3d>>().read();
        assert_eq!(functions.get_id::<DrawMaterial>(), Some(original_id));
        assert_eq!(functions.get_id::<DummyDraw>(), Some(unrelated_id));
        assert_eq!(functions.draw_functions.len(), 2);
        let pointer = |index: usize| {
            functions.draw_functions[index].as_ref() as *const dyn Draw<Opaque3d> as *const ()
        };
        assert_eq!(pointer(unrelated_id.0 as usize), unrelated_ptr);
        assert_ne!(pointer(original_id.0 as usize), original_ptr);
        assert_eq!(
            world.resource::<DrawCountBridge>().0.wrapped_variants[0].load(Ordering::Relaxed),
            1
        );

        type PreservedMaterialCommands = (
            SetItemPipeline,
            SetMeshViewBindGroup<0>,
            SetMeshViewBindingArrayBindGroup<1>,
            SetMeshBindGroup<2>,
            SetMaterialBindGroup<3>,
            CountedDrawMesh,
        );
        assert_eq!(
            TypeId::of::<<DrawMaterial as WithObservedMesh>::Observed>(),
            TypeId::of::<PreservedMaterialCommands>()
        );
    }

    #[test]
    fn completed_frame_deltas_survive_async_delivery_without_replaying() {
        let mut world = World::new();
        world.init_resource::<DrawCountBridge>();
        world.init_resource::<ProfilingState>();
        let bridge = world.resource::<DrawCountBridge>().clone();
        bridge.0.render_schedule_frames.store(1, Ordering::Relaxed);
        bridge.0.counts[0]
            .fixed_indexed_argument_slots
            .store(7, Ordering::Relaxed);
        world.run_system_once(publish_draw_counts).unwrap();
        bridge.0.render_schedule_frames.store(2, Ordering::Relaxed);
        bridge.0.counts[0]
            .fixed_indexed_argument_slots
            .store(10, Ordering::Relaxed);
        world.run_system_once(publish_draw_counts).unwrap();
        let published = bridge.1.lock().unwrap();
        assert_eq!(published.frames.len(), 2);
        assert_eq!(
            published.frames[0].phases["opaque"].fixed_indexed_argument_slots,
            7
        );
        assert_eq!(
            published.frames[1].phases["opaque"].fixed_indexed_argument_slots,
            3
        );
        assert_eq!(
            published.totals.as_ref().unwrap().phases[0]
                .counts
                .fixed_indexed_argument_slots,
            10
        );
        drop(published);
        world.run_system_once(sync_draw_counts).unwrap();
        assert!(bridge.1.lock().unwrap().frames.is_empty());
        let directory = tempfile::tempdir().unwrap();
        let config = crate::config::EngineConfig {
            profile_output_dir: Some(directory.path().to_owned()),
            ..Default::default()
        };
        world
            .resource::<ProfilingState>()
            .write_bundle(
                &config,
                &serde_json::json!({}),
                None,
                &crate::render::RendererMetrics::default(),
                None,
            )
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("draw-submission.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(report["render_schedule_frames"], 2);
        assert_eq!(report["frames"].as_array().unwrap().len(), 2);
        assert_eq!(
            report["frames"][1]["phases"]["opaque"]["fixed_indexed_argument_slots"],
            3
        );
        world.run_system_once(sync_draw_counts).unwrap();
        assert!(bridge.1.lock().unwrap().frames.is_empty());
    }

    #[test]
    fn issued_calls_distinguish_fixed_slots_direct_calls_and_unknown_classification() {
        let counts = AtomicDrawCounts::default();
        record_issued_draw(&counts, Some(true), PhaseItemExtraIndex::None);
        record_issued_draw(&counts, Some(false), PhaseItemExtraIndex::None);
        record_issued_draw(
            &counts,
            Some(true),
            PhaseItemExtraIndex::IndirectParametersIndex {
                range: 12..19,
                batch_set_index: None,
            },
        );
        record_issued_draw(
            &counts,
            Some(false),
            PhaseItemExtraIndex::IndirectParametersIndex {
                range: 20..22,
                batch_set_index: None,
            },
        );
        record_issued_draw(
            &counts,
            Some(true),
            PhaseItemExtraIndex::IndirectParametersIndex {
                range: 25..25,
                batch_set_index: None,
            },
        );
        record_issued_draw(&counts, None, PhaseItemExtraIndex::None);
        let snapshot = counts.snapshot();
        assert_eq!(snapshot.issued_api_calls, 6);
        assert_eq!(snapshot.direct_indexed_api_calls, 1);
        assert_eq!(snapshot.direct_non_indexed_api_calls, 1);
        assert_eq!(snapshot.fixed_indexed_argument_slots, 7);
        assert_eq!(snapshot.fixed_non_indexed_argument_slots, 2);
        assert_eq!(snapshot.empty_fixed_indirect_ranges, 1);
        assert_eq!(snapshot.unclassified_successes, 1);
    }

    #[test]
    fn coverage_reports_missing_and_extra_variants() {
        let bridge = DrawCountBridge::default();
        bridge.0.registry_present[0].store(1, Ordering::Relaxed);
        bridge.0.registered_variants[0].store(2, Ordering::Relaxed);
        bridge.0.wrapped_variants[0].store(1, Ordering::Relaxed);
        let report = bridge.snapshot();
        assert!(!report.coverage_complete);
        assert_eq!(report.phases[0].unwrapped_draw_variants, 1);
        assert!(!report.phases[0].coverage_complete);
        assert!(!report.phases[1].registry_present);
        assert!(report.phases[1].coverage_complete);
    }

    #[test]
    fn gpu_counted_ranges_are_upper_bounds_and_not_fixed_slots() {
        let counts = AtomicDrawCounts::default();
        record_issued_draw(
            &counts,
            Some(true),
            PhaseItemExtraIndex::IndirectParametersIndex {
                range: 100..119,
                batch_set_index: Some(Default::default()),
            },
        );
        let snapshot = counts.snapshot();
        assert_eq!(snapshot.issued_api_calls, 1);
        assert_eq!(snapshot.dynamic_indirect_api_calls, 1);
        assert_eq!(snapshot.dynamic_argument_slot_upper_bound, 19);
        assert_eq!(snapshot.fixed_indirect_api_calls, 0);
        assert_eq!(snapshot.fixed_indexed_argument_slots, 0);
    }
}
