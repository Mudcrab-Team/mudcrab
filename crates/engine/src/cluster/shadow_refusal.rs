//! Terminal GPU failures feed the existing batch resource-failure ledger.
use super::*;
use bevy::render::MainWorld;
use std::collections::{BTreeMap, BTreeSet};

/// Match PipelineCache's own retry classification. Missing shader assets/imports
/// are dependency waits even when the cache temporarily represents them as Err.
pub(super) fn terminal_pipeline_failure(state: &CachedPipelineState) -> Option<String> {
    use bevy::shader::ShaderCacheError;
    match state {
        CachedPipelineState::Err(
            ShaderCacheError::ShaderNotLoaded(_) | ShaderCacheError::ShaderImportNotYetAvailable,
        ) => None,
        CachedPipelineState::Err(error) => Some(format!("{error:?}")),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct ShadowRefusals {
    stamp: Option<shadow_warmup::ShadowPlanStamp>,
    required: HashMap<RetainedViewEntity, HashMap<CachedRenderPipelineId, BTreeSet<usize>>>,
    pipeline_revision: Option<u64>,
    failed: BTreeMap<usize, String>,
    reported: BTreeSet<usize>,
}

impl ShadowRefusals {
    pub(super) fn begin_plan(&mut self, stamp: shadow_warmup::ShadowPlanStamp) {
        if self.stamp == Some(stamp) {
            return;
        }
        if self.stamp.is_none_or(|old| old.gpu != stamp.gpu) {
            self.failed.clear();
            self.reported.clear();
        }
        // An old material's pending pipeline is no longer a dependency after
        // replacement. Terminal failures stay terminal for this GPU scene.
        self.required.clear();
        self.pipeline_revision = None;
        self.stamp = Some(stamp);
    }

    pub(super) fn retain_views(&mut self, views: impl Iterator<Item = RetainedViewEntity>) {
        let views: std::collections::HashSet<_> = views.collect();
        let before = self.required.len();
        self.required.retain(|key, _| views.contains(key));
        if self.required.len() != before {
            self.pipeline_revision = None;
        }
    }

    pub(super) fn begin_view(&mut self, view: RetainedViewEntity) {
        self.required.remove(&view);
        self.pipeline_revision = None;
    }

    pub(super) fn require(
        &mut self,
        view: RetainedViewEntity,
        batch: usize,
        pipeline: CachedRenderPipelineId,
    ) {
        if self
            .required
            .entry(view)
            .or_default()
            .entry(pipeline)
            .or_default()
            .insert(batch)
        {
            self.pipeline_revision = None;
        }
    }

    pub(super) fn is_failed(&self, batch: usize) -> bool {
        self.failed.contains_key(&batch)
    }

    pub(super) fn refuse(&mut self, batch: usize, reason: String) {
        if let std::collections::btree_map::Entry::Vacant(entry) = self.failed.entry(batch) {
            error!(batch, %reason, "cluster shadow batch refused; stock replacement remains disabled");
            entry.insert(reason);
        }
    }

    pub(super) fn refresh(
        &mut self,
        revision: u64,
        mut failure: impl FnMut(CachedRenderPipelineId) -> Option<String>,
    ) {
        if self.pipeline_revision == Some(revision) {
            return;
        }
        self.pipeline_revision = Some(revision);
        let failed: Vec<_> = self
            .required
            .values()
            .flat_map(|pipelines| pipelines.iter())
            .flat_map(|(&pipeline, batches)| {
                failure(pipeline).into_iter().flat_map(move |reason| {
                    batches.iter().map(move |&batch| (batch, reason.clone()))
                })
            })
            .collect();
        for (batch, reason) in failed {
            self.refuse(batch, reason);
        }
    }
}

/// Publication during extraction is scoped as strictly as loading snapshots.
/// A GPU error never changes certification, source authority or frozen ownership.
pub(super) fn publish(
    mut pipelines: ResMut<ClusterPipelines>,
    gpu: Option<Res<ClusterGpu>>,
    scene: Option<Res<ClusterScene>>,
    mut main: ResMut<MainWorld>,
) {
    let (Some(gpu), Some(scene)) = (gpu, scene) else {
        return;
    };
    publish_scoped(
        &mut pipelines.shadow_refusals,
        gpu.identity,
        gpu.certificate.as_ref(),
        &scene,
        &mut main,
    );
}

fn publish_scoped(
    refusals: &mut ShadowRefusals,
    gpu: u64,
    package: Option<&Arc<super::super::reader::CertifiedPackage>>,
    scene: &ClusterScene,
    main: &mut MainWorld,
) {
    if refusals.stamp.is_none_or(|stamp| stamp.gpu != gpu)
        || refusals.failed.len() == refusals.reported.len()
    {
        return;
    }
    let (Some(package), Some(inventory)) = (package, &scene.inventory) else {
        return;
    };
    let coherent = scene
        .certificate
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, package))
        && main
            .get_resource::<super::super::package::ValidatedPackage>()
            .is_some_and(|current| Arc::ptr_eq(&current.0, package))
        && main
            .get_resource::<super::super::package::LazyBatches>()
            .is_some_and(|lazy| inventory.matches(package, lazy.layout_scope))
        && main.get_resource::<ClusterScene>().is_some_and(|current| {
            current
                .certificate
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, package))
                && current
                    .inventory
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, inventory))
        });
    if !coherent {
        return;
    }
    // App::new registers AppExit. Keep an unacknowledged refusal if an isolated
    // World has not registered the existing terminal error channel yet.
    if !main.contains_resource::<Messages<AppExit>>() {
        return;
    }
    let first_refusal = refusals.reported.is_empty();
    let mut terminal_reason = None;
    {
        let Some(mut lazy) = main.get_resource_mut::<super::super::package::LazyBatches>() else {
            return;
        };
        for (&batch, reason) in &refusals.failed {
            if !refusals.reported.contains(&batch) && lazy.refuse_gpu_batch(batch) {
                refusals.reported.insert(batch);
                terminal_reason.get_or_insert_with(|| {
                    format!("cluster shadow batch {batch} refused: {reason}")
                });
            }
        }
    }
    if first_refusal && let Some(reason) = terminal_reason {
        // Accepted renderer failures share the cell/destination terminal owner.
        // Batch refusal and its revision above still commit when another cause won first.
        crate::streaming::failure::refuse_loading(main, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(gpu: u64) -> shadow_warmup::ShadowPlanStamp {
        shadow_warmup::ShadowPlanStamp {
            gpu,
            generation: 1,
            layout_epoch: 1,
            batch_material_version: 1,
            source_layout: 1,
            batch_count: 3,
            layouts_ready: true,
        }
    }

    fn view() -> RetainedViewEntity {
        RetainedViewEntity::new(Entity::PLACEHOLDER.into(), None, 0)
    }

    #[test]
    fn shader_dependencies_remain_pending_but_validation_errors_are_terminal() {
        use bevy::shader::{Shader, ShaderCacheError};
        let pending = [
            CachedPipelineState::Queued,
            CachedPipelineState::Err(ShaderCacheError::ShaderImportNotYetAvailable),
            CachedPipelineState::Err(ShaderCacheError::ShaderNotLoaded(bevy::asset::AssetId::<
                Shader,
            >::default())),
        ];
        for state in &pending {
            assert!(terminal_pipeline_failure(state).is_none());
        }
        let failed = CachedPipelineState::Err(ShaderCacheError::CreateShaderModule(
            "invalid cutout shader".into(),
        ));
        assert!(
            terminal_pipeline_failure(&failed)
                .unwrap()
                .contains("invalid cutout shader")
        );
    }

    #[test]
    fn pending_shadow_pipelines_are_not_terminal_failures() {
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        refusals.require(view(), 1, CachedRenderPipelineId::new(7));
        refusals.refresh(1, |_| None);
        assert!(!refusals.is_failed(1));
        refusals.refresh(1, |_| panic!("unchanged cache rescanned"));
    }

    #[test]
    fn compiler_failure_refuses_exact_batches_once_and_never_retries() {
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        let failed = CachedRenderPipelineId::new(7);
        refusals.require(view(), 0, failed);
        refusals.require(view(), 2, failed);
        refusals.require(view(), 1, CachedRenderPipelineId::new(8));
        refusals.refresh(1, |id| {
            (id == failed).then(|| "invalid shadow shader".into())
        });
        assert_eq!(refusals.failed.keys().copied().collect::<Vec<_>>(), [0, 2]);
        assert_eq!(refusals.failed[&0], "invalid shadow shader");
        refusals.refresh(2, |_| None);
        assert!(refusals.is_failed(0));
        refusals.refuse(0, "must not overwrite the original diagnostic".into());
        assert_eq!(refusals.failed[&0], "invalid shadow shader");
        let mut next = stamp(1);
        next.batch_material_version += 1;
        refusals.begin_plan(next);
        assert!(refusals.is_failed(0));
        refusals.begin_plan(stamp(2));
        assert!(refusals.failed.is_empty());
    }

    #[test]
    fn replaced_material_cannot_inherit_an_old_pending_pipeline_failure() {
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        refusals.require(view(), 1, CachedRenderPipelineId::new(7));
        refusals.refresh(1, |_| None);
        let mut next = stamp(1);
        next.batch_material_version += 1;
        refusals.begin_plan(next);
        let current = CachedRenderPipelineId::new(8);
        refusals.require(view(), 1, current);
        refusals.refresh(2, |id| {
            assert_eq!(id, current);
            None
        });
        assert!(!refusals.is_failed(1));
    }

    #[test]
    fn a_new_dependency_is_checked_even_at_the_same_pipeline_revision() {
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        refusals.refresh(1, |_| None);
        refusals.require(view(), 1, CachedRenderPipelineId::new(7));
        refusals.refresh(1, |_| Some("compile failure".into()));
        assert!(refusals.is_failed(1));
    }

    #[test]
    fn removed_views_and_unused_warmup_variants_cannot_refuse_batches() {
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        refusals.refresh(1, |_| panic!("unused warmup is not a view dependency"));
        refusals.require(view(), 1, CachedRenderPipelineId::new(7));
        refusals.retain_views(std::iter::empty());
        refusals.refresh(1, |_| panic!("removed view cannot refuse a batch"));
        assert!(!refusals.is_failed(1));
        refusals.require(view(), 1, CachedRenderPipelineId::new(8));
        refusals.begin_view(view());
        refusals.refresh(1, |_| panic!("rebuilt plan cannot keep an old variant"));
        assert!(!refusals.is_failed(1));
    }

    #[test]
    fn scoped_gpu_refusal_is_terminal_once_and_preserves_frozen_ownership() {
        use crate::cluster::{
            instance_inventory::InstanceInventory,
            ownership::OwnershipFacts,
            package::{LazyBatches, ValidatedPackage},
        };
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let lazy = LazyBatches::unrequested_for_test(3, Vec3::ZERO);
        let layout = lazy.layout_scope;
        let inventory =
            Arc::new(InstanceInventory::build(package.clone(), layout, &[], 3).unwrap());
        let mesh = ClusterMeshAsset::from_bytes(
            include_bytes!("../../../shared/tests/data/cluster/fixture.mcl").to_vec(),
        )
        .unwrap();
        let mut scene = ClusterScene::new(mesh, &[]);
        scene.certificate = Some(package.clone());
        scene.inventory = Some(inventory.clone());
        let mut main = MainWorld::default();
        main.insert_resource(lazy);
        main.insert_resource(ValidatedPackage(package.clone()));
        main.insert_resource(Messages::<AppExit>::default());
        main.insert_resource(scene.clone());
        let mut facts = OwnershipFacts::default();
        facts.publish(
            &package,
            package.records().map(|(key, _)| (*key, false)).collect(),
        );
        let terminal = facts.terminal_revision();
        let refusal = facts.refusal_revision();
        main.insert_resource(facts);
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        refusals.refuse(1, "invalid cutout shader".into());

        // Stale GPU, scene inventory and layout reports have no effects.
        publish_scoped(&mut refusals, 2, Some(&package), &scene, &mut main);
        main.resource_mut::<LazyBatches>().layout_scope += 1;
        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        main.resource_mut::<LazyBatches>().layout_scope = layout;
        let mut stale = scene.clone();
        stale.inventory = Some(Arc::new(
            InstanceInventory::build(package.clone(), layout, &[], 3).unwrap(),
        ));
        publish_scoped(&mut refusals, 1, Some(&package), &stale, &mut main);
        let other_package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        main.insert_resource(ValidatedPackage(other_package));
        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        main.insert_resource(ValidatedPackage(package.clone()));
        main.remove_resource::<Messages<AppExit>>();
        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        assert!(refusals.reported.is_empty());
        main.insert_resource(Messages::<AppExit>::default());
        assert!(main.resource::<LazyBatches>().failed_batches().is_empty());
        assert!(main.resource::<Messages<AppExit>>().is_empty());

        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        assert_eq!(main.resource::<LazyBatches>().failed_batches(), [1]);
        let exits: Vec<_> = main
            .resource::<Messages<AppExit>>()
            .iter_current_update_messages()
            .cloned()
            .collect();
        assert_eq!(exits, [AppExit::error()]);
        assert_eq!(
            main.resource::<OwnershipFacts>().terminal_revision(),
            terminal
        );
        assert_eq!(
            main.resource::<OwnershipFacts>().refusal_revision(),
            refusal
        );
        assert!(
            main.resource::<OwnershipFacts>()
                .permanent_refused_records()
                .is_empty()
        );
        assert!(
            main.resource::<ValidatedPackage>()
                .0
                .record(package.records().next().unwrap().0)
                .is_some()
        );
    }

    // Exercises the real renderer publication seam, including its existing direct exit.
    fn shadow_fixture() -> (
        tempfile::TempDir,
        Arc<crate::cluster::reader::CertifiedPackage>,
        ClusterScene,
        MainWorld,
    ) {
        use crate::cluster::{
            instance_inventory::InstanceInventory,
            package::{LazyBatches, ValidatedPackage},
        };
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let lazy = LazyBatches::unrequested_for_test(3, Vec3::ZERO);
        let inventory =
            Arc::new(InstanceInventory::build(package.clone(), lazy.layout_scope, &[], 3).unwrap());
        let mesh = ClusterMeshAsset::from_bytes(
            include_bytes!("../../../shared/tests/data/cluster/fixture.mcl").to_vec(),
        )
        .unwrap();
        let mut scene = ClusterScene::new(mesh, &[]);
        scene.certificate = Some(package.clone());
        scene.inventory = Some(inventory);
        let mut main = MainWorld::default();
        main.insert_resource(lazy);
        main.insert_resource(ValidatedPackage(package.clone()));
        main.insert_resource(Messages::<AppExit>::default());
        main.insert_resource(scene.clone());
        (directory, package, scene, main)
    }

    #[test]
    fn accepted_shadow_failure_uses_shared_refusal_and_exactly_one_error_exit() {
        use crate::{cluster::package::LazyBatches, streaming::failure::LoadingRefusal};
        let (_directory, package, scene, mut main) = shadow_fixture();
        let before = main.resource::<LazyBatches>().failed_key();
        let mut exits = main.resource::<Messages<AppExit>>().get_cursor_current();
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        let pipeline = CachedRenderPipelineId::new(7);
        refusals.require(view(), 1, pipeline);
        refusals.refresh(1, |_| Some("invalid shadow variant causal fixture".into()));
        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        main.flush();
        assert_eq!(main.resource::<LazyBatches>().failed_batches(), [1]);
        assert_eq!(
            main.resource::<LazyBatches>().failed_key(),
            (before.0, before.1 + 1)
        );
        let first = main.resource::<LoadingRefusal>().0.clone();
        assert!(!first.is_empty());
        let emitted: Vec<_> = exits
            .read(main.resource::<Messages<AppExit>>())
            .cloned()
            .collect();
        assert_eq!(emitted, [AppExit::error()]);
        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        main.flush();
        assert_eq!(main.resource::<LoadingRefusal>().0, first);
        assert_eq!(
            main.resource::<LazyBatches>().failed_key(),
            (before.0, before.1 + 1)
        );
        assert_eq!(exits.read(main.resource::<Messages<AppExit>>()).count(), 0);
    }

    #[test]
    fn shadow_failure_after_a_lower_failure_preserves_original_reason_and_exit_count() {
        use crate::streaming::failure::{LoadingRefusal, refuse_loading};
        let (_directory, package, scene, mut main) = shadow_fixture();
        let mut exits = main.resource::<Messages<AppExit>>().get_cursor_current();
        refuse_loading(&mut main, "original cell request failure".into());
        let mut refusals = ShadowRefusals::default();
        refusals.begin_plan(stamp(1));
        refusals.refuse(1, "later shadow variant failure".into());
        publish_scoped(&mut refusals, 1, Some(&package), &scene, &mut main);
        main.flush();
        assert_eq!(
            main.resource::<LoadingRefusal>().0,
            "original cell request failure"
        );
        let emitted: Vec<_> = exits
            .read(main.resource::<Messages<AppExit>>())
            .cloned()
            .collect();
        assert_eq!(emitted, [AppExit::error()]);
    }
}
