//! Exact retained NonMesh carrier proof for cluster drawability before QueueMeshes.

use super::*;
use bevy::render::render_phase::{BinnedPhaseItem, BinnedRenderPhase};

/// Read only the custom carrier bins; stock mesh bins never participate in this proof.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct QueuedPhases<'w> {
    pub(super) opaque: Option<Res<'w, ViewBinnedRenderPhases<Opaque3d>>>,
    pub(super) alpha: Option<Res<'w, ViewBinnedRenderPhases<AlphaMask3d>>>,
    pub(super) depth: Option<Res<'w, ViewBinnedRenderPhases<Opaque3dPrepass>>>,
    pub(super) alpha_depth: Option<Res<'w, ViewBinnedRenderPhases<AlphaMask3dPrepass>>>,
    pub(super) shadow: Option<Res<'w, ViewBinnedRenderPhases<Shadow>>>,
}

impl QueuedPhases<'_> {
    fn view(&self, key: &RetainedViewEntity) -> QueuedViewPhases<'_> {
        QueuedViewPhases {
            opaque: self.opaque.as_ref().and_then(|phases| phases.get(key)),
            alpha: self.alpha.as_ref().and_then(|phases| phases.get(key)),
            depth: self.depth.as_ref().and_then(|phases| phases.get(key)),
            alpha_depth: self.alpha_depth.as_ref().and_then(|phases| phases.get(key)),
            shadow: self.shadow.as_ref().and_then(|phases| phases.get(key)),
        }
    }
}

/// Main/shadow and prepass membership stay separate: materials can disable their prepass.
/// The caller combines these with current GPU/material readiness on its readiness cache miss.
pub(super) struct QueuedBatchProof {
    pub(super) revision: u64,
    pub(super) main: Arc<Vec<bool>>,
    pub(super) prepass: Arc<Vec<bool>>,
}

pub(super) fn queued_batch_proof(
    view: &mut ViewGpu,
    batch_count: usize,
    key: &RetainedViewEntity,
    shadow: bool,
    phases: &QueuedPhases,
) -> Arc<QueuedBatchProof> {
    let plan = QueuedProofPlan {
        revision: view.readiness_revision,
        plan_epoch: view.plan_epoch,
        prepass_epoch: view.prepass_epoch,
        batches: batch_count,
        table: &view.table,
        draws: &view.draws,
        representatives: &view.plan_reps,
        positions: &view.plan_rep_positions,
        carriers: &view.walk_carriers,
    };
    view.queued_proof_cache
        .resolve(plan, shadow, phases.view(key))
}

pub(super) fn table_group(table: &[u8], batch: usize) -> Option<u32> {
    let first = batch.checked_mul(16)?;
    let row = table.get(first..first.checked_add(16)?)?;
    if u32::from_le_bytes(row[..4].try_into().ok()?) == 0 {
        return None;
    }
    Some(u32::from_le_bytes(row[4..8].try_into().ok()?))
}

struct QueuedProofPlan<'a> {
    revision: u64,
    plan_epoch: u64,
    prepass_epoch: Option<u64>,
    batches: usize,
    table: &'a [u8],
    draws: &'a HashMap<u32, GroupDraw>,
    representatives: &'a [ReadyBatch],
    positions: &'a HashMap<u32, usize>,
    carriers: &'a [Option<(Entity, MainEntity)>],
}

struct QueuedViewPhases<'a> {
    opaque: Option<&'a BinnedRenderPhase<Opaque3d>>,
    alpha: Option<&'a BinnedRenderPhase<AlphaMask3d>>,
    depth: Option<&'a BinnedRenderPhase<Opaque3dPrepass>>,
    alpha_depth: Option<&'a BinnedRenderPhase<AlphaMask3dPrepass>>,
    shadow: Option<&'a BinnedRenderPhase<Shadow>>,
}

impl QueuedViewPhases<'_> {
    /// Occupancy detects phase withdrawal without walking bins. Replacement authority is the
    /// queue revision, not these lengths: an equal-count replacement still invalidates proof.
    fn occupancy(&self) -> [Option<usize>; 5] {
        fn bins<BPI: BinnedPhaseItem>(phase: Option<&BinnedRenderPhase<BPI>>) -> Option<usize> {
            phase.map(|phase| phase.non_mesh_items.len())
        }
        [
            bins(self.opaque),
            bins(self.alpha),
            bins(self.depth),
            bins(self.alpha_depth),
            bins(self.shadow),
        ]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct QueuedProofKey {
    revision: u64,
    plan_epoch: u64,
    prepass_epoch: Option<u64>,
    batches: usize,
    shadow: bool,
    occupancy: [Option<usize>; 5],
}

#[derive(Default)]
pub(crate) struct QueuedProofCache {
    cached: Option<(QueuedProofKey, Arc<QueuedBatchProof>)>,
    rebuilds: u64,
}

impl QueuedProofCache {
    fn resolve(
        &mut self,
        plan: QueuedProofPlan,
        shadow: bool,
        phases: QueuedViewPhases,
    ) -> Arc<QueuedBatchProof> {
        let key = QueuedProofKey {
            revision: plan.revision,
            plan_epoch: plan.plan_epoch,
            prepass_epoch: plan.prepass_epoch,
            batches: plan.batches,
            shadow,
            occupancy: phases.occupancy(),
        };
        if let Some((cached, proof)) = &self.cached
            && *cached == key
        {
            return Arc::clone(proof);
        }

        // Only the three cluster queues write production NonMesh items, and each replacement
        // bumps ViewGpu::readiness_revision (including draws/table writes in DrawGroups::apply).
        // Bevy frame preparation retains these bins. Terrain removal names source terrain
        // entities, never these carrier-only entities. Any future carrier writer must also
        // advance that revision; phase lengths cannot certify an equal-count replacement.
        #[cfg(test)]
        crate::work_counters::record("cluster_select/queued_proof");
        self.rebuilds = self.rebuilds.wrapping_add(1);
        let mut main = vec![false; plan.batches];
        let mut prepass = vec![false; plan.batches];
        if shadow {
            let members = non_mesh_members(phases.shadow);
            for (batch, queued) in main.iter_mut().enumerate() {
                let Some(group) = table_group(plan.table, batch) else {
                    continue;
                };
                if plan.draws.contains_key(&group) {
                    *queued = plan
                        .carriers
                        .get(group as usize)
                        .copied()
                        .flatten()
                        .is_some_and(|carrier| members.contains(&carrier));
                }
            }
        } else {
            let forward = [
                non_mesh_members(phases.opaque),
                non_mesh_members(phases.alpha),
            ];
            let depth = [
                non_mesh_members(phases.depth),
                non_mesh_members(phases.alpha_depth),
            ];
            for (batch, (main, prepass)) in main.iter_mut().zip(&mut prepass).enumerate() {
                let representative = table_group(plan.table, batch)
                    .and_then(|group| plan.draws.get(&group))
                    .and_then(|draw| plan.positions.get(&draw.representative))
                    .and_then(|position| plan.representatives.get(*position));
                let Some(representative) = representative else {
                    continue;
                };
                let pair = (representative.render_entity, representative.main_entity);
                let phase = usize::from(representative.alpha_mask);
                *main = forward[phase].contains(&pair);
                *prepass = depth[phase].contains(&pair);
            }
        }
        let proof = Arc::new(QueuedBatchProof {
            revision: self.rebuilds,
            main: Arc::new(main),
            prepass: Arc::new(prepass),
        });
        self.cached = Some((key, Arc::clone(&proof)));
        proof
    }
}

fn non_mesh_members<BPI: BinnedPhaseItem>(
    phase: Option<&BinnedRenderPhase<BPI>>,
) -> HashSet<(Entity, MainEntity)> {
    phase
        .into_iter()
        .flat_map(|phase| phase.non_mesh_items.values())
        .flat_map(|bin| bin.entities.iter().map(|(main, render)| (*render, *main)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::{
        batching::gpu_preprocessing::GpuPreprocessingMode,
        render_phase::{BinnedPhaseItem, DrawFunctionId},
    };

    fn entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).unwrap()
    }

    fn main(index: u32) -> MainEntity {
        entity(index).into()
    }

    fn key() -> RetainedViewEntity {
        RetainedViewEntity::new(main(100), None, 0)
    }

    fn add<BPI: BinnedPhaseItem>(
        phases: &mut ViewBinnedRenderPhases<BPI>,
        batch: BPI::BatchSetKey,
        bin: BPI::BinKey,
        pair: (Entity, MainEntity),
    ) {
        phases.prepare_for_new_frame(key(), GpuPreprocessingMode::None);
        phases.get_mut(&key()).unwrap().add(
            batch,
            bin,
            pair,
            InputUniformIndex::default(),
            BinnedRenderPhaseType::NonMesh,
        );
    }

    fn opaque_key() -> Opaque3dBatchSetKey {
        Opaque3dBatchSetKey {
            pipeline: CachedRenderPipelineId::INVALID,
            draw_function: DrawFunctionId(0),
            material_bind_group_index: None,
            lightmap_slab: None,
            slabs: MeshSlabs::default(),
        }
    }

    fn depth_key() -> OpaqueNoLightmap3dBatchSetKey {
        OpaqueNoLightmap3dBatchSetKey {
            pipeline: CachedRenderPipelineId::INVALID,
            draw_function: DrawFunctionId(0),
            material_bind_group_index: None,
            slabs: MeshSlabs::default(),
        }
    }

    #[derive(Default)]
    struct Phases {
        opaque: ViewBinnedRenderPhases<Opaque3d>,
        alpha: ViewBinnedRenderPhases<AlphaMask3d>,
        depth: ViewBinnedRenderPhases<Opaque3dPrepass>,
        alpha_depth: ViewBinnedRenderPhases<AlphaMask3dPrepass>,
        shadow: ViewBinnedRenderPhases<Shadow>,
    }

    impl Phases {
        fn view(&self) -> QueuedViewPhases<'_> {
            QueuedViewPhases {
                opaque: self.opaque.get(&key()),
                alpha: self.alpha.get(&key()),
                depth: self.depth.get(&key()),
                alpha_depth: self.alpha_depth.get(&key()),
                shadow: self.shadow.get(&key()),
            }
        }

        fn forward(&mut self, masked: bool, pair: (Entity, MainEntity)) {
            if masked {
                add(
                    &mut self.alpha,
                    depth_key(),
                    OpaqueNoLightmap3dBinKey {
                        asset_id: batch_asset_id(0),
                    },
                    pair,
                );
            } else {
                add(
                    &mut self.opaque,
                    opaque_key(),
                    Opaque3dBinKey {
                        asset_id: batch_asset_id(0),
                    },
                    pair,
                );
            }
        }

        fn prepass(&mut self, masked: bool, pair: (Entity, MainEntity)) {
            let bin = OpaqueNoLightmap3dBinKey {
                asset_id: batch_asset_id(0),
            };
            if masked {
                add(&mut self.alpha_depth, depth_key(), bin, pair);
            } else {
                add(&mut self.depth, depth_key(), bin, pair);
            }
        }
    }

    struct Plan {
        table: Vec<u8>,
        draws: HashMap<u32, GroupDraw>,
        representatives: Vec<ReadyBatch>,
        positions: HashMap<u32, usize>,
        carriers: Vec<Option<(Entity, MainEntity)>>,
    }

    impl Plan {
        fn new(masked: bool) -> Self {
            let mut table = Vec::new();
            // Two batches share a representative whose batch index differs from its group.
            push_u32(&mut table, &[1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
            Self {
                table,
                draws: HashMap::from([(
                    0,
                    GroupDraw {
                        representative: 1,
                        args_offset: 0,
                        count_offset: 0,
                        max_count: 2,
                    },
                )]),
                representatives: vec![ReadyBatch {
                    index: 1,
                    render_entity: entity(11),
                    main_entity: main(1),
                    pipeline: CachedRenderPipelineId::INVALID,
                    material_group: 0,
                    alpha_mask: masked,
                }],
                positions: HashMap::from([(1, 0)]),
                carriers: vec![Some((entity(11), main(1)))],
            }
        }

        fn input(&self, revision: u64) -> QueuedProofPlan<'_> {
            QueuedProofPlan {
                revision,
                plan_epoch: 1,
                prepass_epoch: Some(1),
                batches: 3,
                table: &self.table,
                draws: &self.draws,
                representatives: &self.representatives,
                positions: &self.positions,
                carriers: &self.carriers,
            }
        }
    }

    #[test]
    fn batch_table_group_refuses_missing_disabled_or_truncated_rows() {
        let mut table = Vec::new();
        push_u32(&mut table, &[1, 7, 0, 0, 0, 8, 0, 0]);
        assert_eq!(table_group(&table, 0), Some(7));
        assert_eq!(table_group(&table, 1), None);
        assert_eq!(table_group(&table, 2), None);
        assert_eq!(table_group(&table[..15], 0), None);
        assert_eq!(table_group(&table, usize::MAX), None);
    }

    #[test]
    fn unrelated_non_mesh_carrier_cannot_certify_a_group() {
        let plan = Plan::new(false);
        let mut phases = Phases::default();
        phases.forward(false, (entity(22), main(2)));
        let proof = QueuedProofCache::default().resolve(plan.input(1), false, phases.view());
        assert_eq!(*proof.main, vec![false; 3]);
    }

    #[test]
    fn camera_proof_requires_the_exact_render_pair_and_correct_alpha_phase() {
        for masked in [false, true] {
            let plan = Plan::new(masked);
            let mut cache = QueuedProofCache::default();
            let mut phases = Phases::default();
            phases.forward(!masked, (entity(11), main(1)));
            assert_eq!(
                *cache.resolve(plan.input(1), false, phases.view()).main,
                vec![false; 3]
            );
            phases.forward(masked, (entity(99), main(1)));
            assert_eq!(
                *cache.resolve(plan.input(2), false, phases.view()).main,
                vec![false; 3]
            );
            phases.forward(masked, (entity(11), main(1)));
            assert_eq!(
                *cache.resolve(plan.input(3), false, phases.view()).main,
                vec![true, true, false]
            );
        }
    }

    #[test]
    fn missing_required_prepass_does_not_borrow_forward_or_other_alpha_proof() {
        for masked in [false, true] {
            let plan = Plan::new(masked);
            let mut cache = QueuedProofCache::default();
            let mut phases = Phases::default();
            phases.forward(masked, (entity(11), main(1)));
            phases.prepass(!masked, (entity(11), main(1)));
            let missing = cache.resolve(plan.input(1), false, phases.view());
            assert_eq!(*missing.main, vec![true, true, false]);
            assert_eq!(*missing.prepass, vec![false; 3]);
            phases.prepass(masked, (entity(11), main(1)));
            let mut partial_plan = plan.input(2);
            partial_plan.prepass_epoch = None;
            let complete = cache.resolve(partial_plan, false, phases.view());
            assert_eq!(*complete.prepass, vec![true, true, false]);
        }
    }

    #[test]
    fn shadow_proof_uses_the_group_carrier_without_a_camera_representative() {
        let mut plan = Plan::new(false);
        plan.representatives.clear();
        plan.positions.clear();
        let mut phases = Phases::default();
        phases.forward(false, (entity(11), main(1)));
        let mut cache = QueuedProofCache::default();
        assert_eq!(
            *cache.resolve(plan.input(1), true, phases.view()).main,
            vec![false; 3]
        );
        add(
            &mut phases.shadow,
            ShadowBatchSetKey {
                pipeline: CachedRenderPipelineId::INVALID,
                draw_function: DrawFunctionId(0),
                material_bind_group_index: None,
                slabs: MeshSlabs::default(),
            },
            ShadowBinKey {
                asset_id: batch_asset_id(0),
            },
            (entity(11), main(1)),
        );
        assert_eq!(
            *cache.resolve(plan.input(2), true, phases.view()).main,
            vec![true, true, false]
        );
    }

    #[test]
    fn settled_proof_replays_without_non_mesh_or_batch_work() {
        let plan = Plan::new(false);
        let mut phases = Phases::default();
        phases.forward(false, (entity(11), main(1)));
        let mut cache = QueuedProofCache::default();
        let first = cache.resolve(plan.input(1), false, phases.view());
        for _ in 0..64 {
            phases
                .opaque
                .prepare_for_new_frame(key(), GpuPreprocessingMode::None);
            let replay = cache.resolve(plan.input(1), false, phases.view());
            assert!(Arc::ptr_eq(&first, &replay));
        }
        assert_eq!(cache.rebuilds, 1);
    }

    #[test]
    fn plan_and_prepass_epochs_invalidate_independently_of_occupancy() {
        let plan = Plan::new(false);
        let mut phases = Phases::default();
        phases.forward(false, (entity(11), main(1)));
        let mut cache = QueuedProofCache::default();
        let first = cache.resolve(plan.input(1), false, phases.view());
        let mut next_plan = plan.input(1);
        next_plan.plan_epoch += 1;
        let planned = cache.resolve(next_plan, false, phases.view());
        assert_ne!(first.revision, planned.revision);
        let mut next_prepass = plan.input(1);
        next_prepass.plan_epoch += 1;
        next_prepass.prepass_epoch = None;
        let partial = cache.resolve(next_prepass, false, phases.view());
        assert_ne!(planned.revision, partial.revision);
        assert_eq!(cache.rebuilds, 3);
    }

    #[test]
    fn equal_bin_count_replacement_invalidates_on_the_queue_revision() {
        let plan = Plan::new(false);
        let mut phases = Phases::default();
        phases.forward(false, (entity(11), main(1)));
        let mut cache = QueuedProofCache::default();
        let first = cache.resolve(plan.input(1), false, phases.view());
        phases.opaque.get_mut(&key()).unwrap().remove(main(1));
        phases.forward(false, (entity(22), main(2)));
        assert_eq!(phases.opaque.get(&key()).unwrap().non_mesh_items.len(), 1);
        let replaced = cache.resolve(plan.input(2), false, phases.view());
        assert_ne!(first.revision, replaced.revision);
        assert_eq!(*replaced.main, vec![false; 3]);
        // Phase disappearance also invalidates with an unchanged queue revision.
        phases.opaque.remove(&key());
        let absent = cache.resolve(plan.input(2), false, phases.view());
        assert_ne!(replaced.revision, absent.revision);
        assert_eq!(cache.rebuilds, 3);
    }
}
