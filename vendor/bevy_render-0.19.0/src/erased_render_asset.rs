use crate::{
    render_resource::AsBindGroupError, ExtractSchedule, MainWorld, Render, RenderApp,
    RenderStartup, RenderSystems, Res,
};
use bevy_app::{App, Plugin, SubApp};
use bevy_asset::RenderAssetUsages;
use bevy_asset::{Asset, AssetEvent, AssetId, Assets, UntypedAssetId};
use bevy_ecs::{
    change_detection::DetectChangesMut,
    message::{Message, MessageCursor, Messages},
    prelude::{Commands, IntoScheduleConfigs, Local, MessageReader, ResMut, Resource},
    schedule::{ScheduleConfigs, SystemSet},
    system::{ScheduleSystem, StaticSystemParam, SystemParam, SystemParamItem, SystemState},
    world::{FromWorld, Mut},
};
use bevy_log::{debug, error};
use bevy_platform::collections::{HashMap, HashSet};
use bevy_render::render_asset::RenderAssetBytesPerFrameLimiter;
use core::marker::PhantomData;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrepareAssetError<E: Send + Sync + 'static> {
    #[error("Failed to prepare asset")]
    RetryNextUpdate(E),
    #[error("Failed to build bind group: {0}")]
    AsBindGroupError(AsBindGroupError),
}

/// The system set during which we extract modified assets to the render world.
#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub struct AssetExtractionSystems;

/// Describes how an asset gets extracted and prepared for rendering.
///
/// In the [`ExtractSchedule`] step the [`ErasedRenderAsset::SourceAsset`] is transferred
/// from the "main world" into the "render world".
///
/// After that in the [`RenderSystems::PrepareAssets`] step the extracted asset
/// is transformed into its GPU-representation of type [`ErasedRenderAsset`].
pub trait ErasedRenderAsset: Send + Sync + 'static {
    /// The representation of the asset in the "main world".
    type SourceAsset: Asset + Clone;
    /// The target representation of the asset in the "render world".
    type ErasedAsset: Send + Sync + 'static + Sized;

    /// Specifies all ECS data required by [`ErasedRenderAsset::prepare_asset`].
    ///
    /// For convenience use the [`lifetimeless`](bevy_ecs::system::lifetimeless) [`SystemParam`].
    type Param: SystemParam;

    /// Whether or not to unload the asset after extracting it to the render world.
    #[inline]
    fn asset_usage(_source_asset: &Self::SourceAsset) -> RenderAssetUsages {
        RenderAssetUsages::default()
    }

    /// Size of the data the asset will upload to the gpu. Specifying a return value
    /// will allow the asset to be throttled via [`RenderAssetBytesPerFrameLimiter`].
    #[inline]
    #[expect(
        unused_variables,
        reason = "The parameters here are intentionally unused by the default implementation; however, putting underscores here will result in the underscores being copied by rust-analyzer's tab completion."
    )]
    fn byte_len(erased_asset: &Self::SourceAsset) -> Option<usize> {
        None
    }

    /// Prepares the [`ErasedRenderAsset::SourceAsset`] for the GPU by transforming it into a [`ErasedRenderAsset`].
    ///
    /// ECS data may be accessed via `param`.
    fn prepare_asset(
        source_asset: Self::SourceAsset,
        asset_id: AssetId<Self::SourceAsset>,
        param: &mut SystemParamItem<Self::Param>,
    ) -> Result<Self::ErasedAsset, PrepareAssetError<Self::SourceAsset>>;

    /// Called whenever the [`ErasedRenderAsset::SourceAsset`] has been removed.
    ///
    /// You can implement this method if you need to access ECS data (via
    /// `_param`) in order to perform cleanup tasks when the asset is removed.
    ///
    /// The default implementation does nothing.
    fn unload_asset(
        _source_asset: AssetId<Self::SourceAsset>,
        _param: &mut SystemParamItem<Self::Param>,
    ) {
    }
}

/// This plugin extracts the changed assets from the "app world" into the "render world"
/// and prepares them for the GPU. They can then be accessed from the [`ErasedRenderAssets`] resource.
///
/// Therefore it sets up the [`ExtractSchedule`] and
/// [`RenderSystems::PrepareAssets`] steps for the specified [`ErasedRenderAsset`].
///
/// The `AFTER` generic parameter can be used to specify that `A::prepare_asset` should not be run until
/// `prepare_assets::<AFTER>` has completed. This allows the `prepare_asset` function to depend on another
/// prepared [`ErasedRenderAsset`], for example `Mesh::prepare_asset` relies on `ErasedRenderAssets::<GpuImage>` for morph
/// targets, so the plugin is created as `ErasedRenderAssetPlugin::<RenderMesh, GpuImage>::default()`.
pub struct ErasedRenderAssetPlugin<
    A: ErasedRenderAsset,
    AFTER: ErasedRenderAssetDependency + 'static = (),
> {
    phantom: PhantomData<fn() -> (A, AFTER)>,
}

impl<A: ErasedRenderAsset, AFTER: ErasedRenderAssetDependency + 'static> Default
    for ErasedRenderAssetPlugin<A, AFTER>
{
    fn default() -> Self {
        Self {
            phantom: Default::default(),
        }
    }
}

impl<A: ErasedRenderAsset, AFTER: ErasedRenderAssetDependency + 'static> Plugin
    for ErasedRenderAssetPlugin<A, AFTER>
{
    fn build(&self, app: &mut App) {
        app.init_resource::<CachedExtractErasedRenderAssetSystemState<A>>();
    }

    fn finish(&self, app: &mut App) {
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            let first_collection = !render_app
                .world()
                .contains_resource::<ErasedAssetChangeLogInstalled<A::ErasedAsset>>();
            render_app
                .init_resource::<ExtractedAssets<A>>()
                .init_resource::<ErasedRenderAssets<A::ErasedAsset>>()
                .allow_ambiguous_resource::<ErasedRenderAssets<A::ErasedAsset>>()
                .init_resource::<PrepareNextFrameAssets<A>>()
                .add_systems(
                    RenderStartup,
                    collect_erased_render_assets_to_reextract::<A>,
                )
                .add_systems(
                    ExtractSchedule,
                    extract_erased_render_asset::<A>.in_set(AssetExtractionSystems),
                );
            if first_collection {
                render_app
                    .world_mut()
                    .insert_resource(ErasedAssetChangeLogInstalled::<A::ErasedAsset>(PhantomData));
                render_app.add_systems(
                    Render,
                    advance_erased_asset_changes::<A::ErasedAsset>
                        .in_set(RenderSystems::ExtractCommands),
                );
            }
            AFTER::register_system(
                render_app,
                prepare_erased_assets::<A>.in_set(RenderSystems::PrepareAssets),
            );
        }
    }
}

// helper to allow specifying dependencies between render assets
pub trait ErasedRenderAssetDependency {
    fn register_system(render_app: &mut SubApp, system: ScheduleConfigs<ScheduleSystem>);
}

impl ErasedRenderAssetDependency for () {
    fn register_system(render_app: &mut SubApp, system: ScheduleConfigs<ScheduleSystem>) {
        render_app.add_systems(Render, system);
    }
}

impl<A: ErasedRenderAsset> ErasedRenderAssetDependency for A {
    fn register_system(render_app: &mut SubApp, system: ScheduleConfigs<ScheduleSystem>) {
        render_app.add_systems(Render, system.after(prepare_erased_assets::<A>));
    }
}

/// Temporarily stores the extracted and removed assets of the current frame.
#[derive(Resource)]
pub struct ExtractedAssets<A: ErasedRenderAsset> {
    /// The assets extracted this frame.
    ///
    /// These are assets that were either added or modified this frame.
    pub extracted: Vec<(AssetId<A::SourceAsset>, A::SourceAsset)>,

    /// IDs of the assets that were removed this frame.
    ///
    /// These assets will not be present in [`ExtractedAssets::extracted`].
    pub removed: HashSet<AssetId<A::SourceAsset>>,

    /// IDs of the assets that were modified this frame.
    pub modified: HashSet<AssetId<A::SourceAsset>>,

    /// IDs of the assets that were added this frame.
    pub added: HashSet<AssetId<A::SourceAsset>>,
}

impl<A: ErasedRenderAsset> Default for ExtractedAssets<A> {
    fn default() -> Self {
        Self {
            extracted: Default::default(),
            removed: Default::default(),
            modified: Default::default(),
            added: Default::default(),
        }
    }
}

/// Stores all GPU representations ([`ErasedRenderAsset`])
/// of [`ErasedRenderAsset::SourceAsset`] as long as they exist.
#[derive(Resource)]
pub struct ErasedRenderAssets<ERA>(
    HashMap<UntypedAssetId, ERA>,
    u64,
    HashMap<UntypedAssetId, u64>,
    HashSet<UntypedAssetId>,
    Messages<ErasedAssetMutation>,
    u64,
);

#[derive(Message)]
struct ErasedAssetMutation(UntypedAssetId);

#[derive(Resource)]
struct ErasedAssetChangeLogInstalled<ERA: Send + Sync + 'static>(PhantomData<fn() -> ERA>);

/// An independent prepared-asset mutation cursor.
/// A cursor belongs to one collection lifetime, even when revisions restart.
#[derive(Default)]
pub struct ErasedRenderAssetChangeReader {
    scope: u64,
    revision: u64,
    cursor: MessageCursor<ErasedAssetMutation>,
}

impl<ERA> Default for ErasedRenderAssets<ERA> {
    fn default() -> Self {
        static NEXT_SCOPE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
        let scope = NEXT_SCOPE
            .fetch_update(
                core::sync::atomic::Ordering::Relaxed,
                core::sync::atomic::Ordering::Relaxed,
                |value| value.checked_add(1),
            )
            .expect("Prepared asset collection scope exhausted");
        Self(
            Default::default(),
            0,
            Default::default(),
            Default::default(),
            Messages::default(),
            scope,
        )
    }
}

impl<ERA> ErasedRenderAssets<ERA> {
    /// Identifies this collection's lifetime.
    /// Pair with `revision` when retaining proof across resource replacement.
    pub fn change_scope(&self) -> u64 {
        self.5
    }

    /// Non-destructive, independent mutation observation.
    /// Returns `None` for a new/replaced collection or expired cursor: rebuild the
    /// complete baseline. Otherwise returns deduplicated IDs changed since this
    /// reader last observed, including removals and conservative mutable access.
    /// Messages survive two render update cycles. An unchanged revision visits no
    /// events or assets. This never drains the shadow cache's `take_changed_assets`.
    pub fn read_changes(
        &self,
        reader: &mut ErasedRenderAssetChangeReader,
    ) -> Option<Vec<UntypedAssetId>> {
        if reader.scope != self.5 || reader.cursor.missed_messages(&self.4) != 0 {
            reader.scope = self.5;
            reader.revision = self.1;
            reader.cursor = self.4.get_cursor_current();
            return None;
        }
        if reader.revision == self.1 {
            return Some(Vec::new());
        }
        reader.revision = self.1;
        Some(
            reader
                .cursor
                .read(&self.4)
                .map(|change| change.0)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect(),
        )
    }

    /// Drains producer mutation IDs for a single incremental dependency consumer.
    /// Reads and missing removals do not produce deltas; retries publish on
    /// successful preparation. Use `revision` for independent coarse consumers.
    pub fn take_changed_assets(&mut self) -> Vec<UntypedAssetId> {
        self.3.drain().collect()
    }

    /// Revision of one prepared asset, without invalidation from unrelated materials.
    pub fn asset_revision(&self, id: impl Into<UntypedAssetId>) -> Option<u64> {
        self.2.get(&id.into()).copied()
    }

    /// Returns the monotonic change revision of this collection.
    ///
    /// Insertions, replacements, removals, and mutable access advance the revision.
    /// Immutable access and missing-entry lookups or removals do not. Mutable access
    /// is conservative: a returned mutable reference may change an asset even when
    /// the collection cannot observe the write. The revision starts at zero and is
    /// scoped to this collection's lifetime. Reading it does not scan the assets.
    #[inline]
    pub fn revision(&self) -> u64 {
        self.1
    }

    fn advance_revision(&mut self) {
        self.1 = self
            .1
            .checked_add(1)
            .expect("Render asset revision exhausted");
    }

    pub fn get(&self, id: impl Into<UntypedAssetId>) -> Option<&ERA> {
        self.0.get(&id.into())
    }

    pub fn get_mut(&mut self, id: impl Into<UntypedAssetId>) -> Option<&mut ERA> {
        let id = id.into();
        let asset = self.0.get_mut(&id)?;
        self.1 = self
            .1
            .checked_add(1)
            .expect("Render asset revision exhausted");
        self.2.insert(id, self.1);
        self.3.insert(id);
        self.4.write(ErasedAssetMutation(id));
        Some(asset)
    }

    pub fn insert(&mut self, id: impl Into<UntypedAssetId>, value: ERA) -> Option<ERA> {
        let id = id.into();
        self.advance_revision();
        self.2.insert(id, self.1);
        self.3.insert(id);
        self.4.write(ErasedAssetMutation(id));
        self.0.insert(id, value)
    }

    pub fn remove(&mut self, id: impl Into<UntypedAssetId>) -> Option<ERA> {
        let id = id.into();
        let removed = self.0.remove(&id);
        if removed.is_some() {
            self.advance_revision();
            self.2.remove(&id);
            self.3.insert(id);
            self.4.write(ErasedAssetMutation(id));
        }
        removed
    }

    pub fn iter(&self) -> impl Iterator<Item = (UntypedAssetId, &ERA)> {
        self.0.iter().map(|(k, v)| (*k, v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (UntypedAssetId, &mut ERA)> {
        if !self.0.is_empty() {
            self.advance_revision();
            for revision in self.2.values_mut() {
                *revision = self.1;
            }
            self.3.extend(self.0.keys().copied());
            self.4
                .write_batch(self.0.keys().copied().map(ErasedAssetMutation));
        }
        self.0.iter_mut().map(|(k, v)| (*k, v))
    }
}

/// Rotate once per erased collection, before all material-type preparations.
/// Empty logs need no rotation; no asset population is inspected here.
fn advance_erased_asset_changes<ERA: Send + Sync + 'static>(
    mut assets: ResMut<ErasedRenderAssets<ERA>>,
) {
    if !assets.4.is_empty() {
        assets.bypass_change_detection().4.update();
    }
}

#[derive(Resource)]
struct CachedExtractErasedRenderAssetSystemState<A: ErasedRenderAsset> {
    state: SystemState<(
        MessageReader<'static, 'static, AssetEvent<A::SourceAsset>>,
        ResMut<'static, Assets<A::SourceAsset>>,
    )>,
}

impl<A: ErasedRenderAsset> FromWorld for CachedExtractErasedRenderAssetSystemState<A> {
    fn from_world(world: &mut bevy_ecs::world::World) -> Self {
        Self {
            state: SystemState::new(world),
        }
    }
}

/// Resource inserted during [`RenderStartup`] containing asset IDs that need
/// re-extraction from the main world after device recovery.
#[derive(Resource)]
pub(crate) struct ErasedRenderAssetsToReExtract<A: ErasedRenderAsset> {
    ids: Vec<AssetId<A::SourceAsset>>,
}

/// Drains all asset IDs from [`ErasedRenderAssets<A>`] to mark for re-extraction.
fn collect_erased_render_assets_to_reextract<A: ErasedRenderAsset>(
    mut commands: Commands,
    mut render_assets: ResMut<ErasedRenderAssets<A::ErasedAsset>>,
    mut prepare_next_frame: ResMut<PrepareNextFrameAssets<A>>,
) {
    let source_type_id = core::any::TypeId::of::<A::SourceAsset>();
    // ErasedRenderAssets is shared across all material types that produce
    // the same ErasedAsset type. Drain only the entries matching our SourceAsset.
    let mut ids = Vec::new();
    render_assets.0.retain(|untyped_id, _| {
        if untyped_id.type_id() == source_type_id {
            ids.push(untyped_id.typed());
            false
        } else {
            true
        }
    });
    prepare_next_frame.assets.clear();
    if !ids.is_empty() {
        for id in &ids {
            render_assets.2.remove(&id.untyped());
            render_assets.3.insert(id.untyped());
            render_assets.4.write(ErasedAssetMutation(id.untyped()));
        }
        render_assets.advance_revision();
        commands.insert_resource(ErasedRenderAssetsToReExtract::<A> { ids });
    }
}

/// Extracts all created or modified assets of the corresponding [`ErasedRenderAsset::SourceAsset`] type
/// into the "render world", including any assets invalidated by device recovery.
pub(crate) fn extract_erased_render_asset<A: ErasedRenderAsset>(
    mut to_reextract: Option<ResMut<ErasedRenderAssetsToReExtract<A>>>,
    mut extracted_assets: ResMut<ExtractedAssets<A>>,
    mut main_world: ResMut<MainWorld>,
    mut needs_extracting: Local<HashSet<AssetId<A::SourceAsset>>>,
) {
    extracted_assets.extracted.clear();
    extracted_assets.removed.clear();
    extracted_assets.modified.clear();
    extracted_assets.added.clear();
    needs_extracting.clear();

    let reextract_ids = to_reextract
        .as_mut()
        .map(|r| core::mem::take(&mut r.ids))
        .filter(|ids| !ids.is_empty());

    main_world.resource_scope(
        |world, mut cached_state: Mut<CachedExtractErasedRenderAssetSystemState<A>>| {
            let (mut events, mut assets) = cached_state.state.get_mut(world).unwrap();

            if let Some(reextract_ids) = reextract_ids {
                needs_extracting.extend(reextract_ids);
            }

            for event in events.read() {
                #[expect(
                    clippy::match_same_arms,
                    reason = "LoadedWithDependencies is marked as a TODO, so it's likely this will no longer lint soon."
                )]
                match event {
                    AssetEvent::Added { id } => {
                        needs_extracting.insert(*id);
                    }
                    AssetEvent::Modified { id } => {
                        needs_extracting.insert(*id);
                        extracted_assets.modified.insert(*id);
                    }
                    AssetEvent::Removed { .. } => {
                        // We don't care that the asset was removed from Assets<T> in the main world.
                        // An asset is only removed from ErasedRenderAssets<T> when its last handle is dropped (AssetEvent::Unused).
                    }
                    AssetEvent::Unused { id } => {
                        needs_extracting.remove(id);
                        extracted_assets.modified.remove(id);
                        extracted_assets.removed.insert(*id);
                    }
                    AssetEvent::LoadedWithDependencies { .. } => {
                        // TODO: handle this
                    }
                }
            }

            for id in needs_extracting.drain() {
                if let Some(asset) = assets.get(id) {
                    let asset_usage = A::asset_usage(asset);
                    if asset_usage.contains(RenderAssetUsages::RENDER_WORLD) {
                        if asset_usage == RenderAssetUsages::RENDER_WORLD {
                            if let Some(asset) = assets.remove(id) {
                                extracted_assets.extracted.push((id, asset));
                                extracted_assets.added.insert(id);
                            }
                        } else {
                            extracted_assets.extracted.push((id, asset.clone()));
                            extracted_assets.added.insert(id);
                        }
                    }
                }
            }

            cached_state.state.apply(world);
        },
    );
}

// TODO: consider storing inside system?
/// All assets that should be prepared next frame.
#[derive(Resource)]
pub struct PrepareNextFrameAssets<A: ErasedRenderAsset> {
    assets: Vec<(AssetId<A::SourceAsset>, A::SourceAsset)>,
}

impl<A: ErasedRenderAsset> Default for PrepareNextFrameAssets<A> {
    fn default() -> Self {
        Self {
            assets: Default::default(),
        }
    }
}

/// This system prepares all assets of the corresponding [`ErasedRenderAsset::SourceAsset`] type
/// which where extracted this frame for the GPU.
pub fn prepare_erased_assets<A: ErasedRenderAsset>(
    mut extracted_assets: ResMut<ExtractedAssets<A>>,
    mut render_assets: ResMut<ErasedRenderAssets<A::ErasedAsset>>,
    mut prepare_next_frame: ResMut<PrepareNextFrameAssets<A>>,
    param: StaticSystemParam<<A as ErasedRenderAsset>::Param>,
    bpf: Res<RenderAssetBytesPerFrameLimiter>,
) {
    let mut wrote_asset_count = 0;

    let mut param = param.into_inner();
    let queued_assets = core::mem::take(&mut prepare_next_frame.assets);
    for (id, extracted_asset) in queued_assets {
        if extracted_assets.removed.contains(&id) || extracted_assets.added.contains(&id) {
            // skip previous frame's assets that have been removed or updated
            continue;
        }

        let write_bytes = if let Some(size) = A::byte_len(&extracted_asset) {
            // we could check if available bytes > byte_len here, but we want to make some
            // forward progress even if the asset is larger than the max bytes per frame.
            // this way we always write at least one (sized) asset per frame.
            // in future we could also consider partial asset uploads.
            if bpf.exhausted() {
                prepare_next_frame.assets.push((id, extracted_asset));
                continue;
            }
            size
        } else {
            0
        };

        match A::prepare_asset(extracted_asset, id, &mut param) {
            Ok(prepared_asset) => {
                render_assets.insert(id, prepared_asset);
                bpf.write_bytes(write_bytes);
                wrote_asset_count += 1;
            }
            Err(PrepareAssetError::RetryNextUpdate(extracted_asset)) => {
                prepare_next_frame.assets.push((id, extracted_asset));
            }
            Err(PrepareAssetError::AsBindGroupError(e)) => {
                error!(
                    "{} Bind group construction failed: {e}",
                    core::any::type_name::<A>()
                );
            }
        }
    }

    for removed in extracted_assets.removed.drain() {
        render_assets.remove(removed);
        A::unload_asset(removed, &mut param);
    }

    for (id, extracted_asset) in extracted_assets.extracted.drain(..) {
        // we remove previous here to ensure that if we are updating the asset then
        // any users will not see the old asset after a new asset is extracted,
        // even if the new asset is not yet ready or we are out of bytes to write.
        render_assets.remove(id);

        let write_bytes = if let Some(size) = A::byte_len(&extracted_asset) {
            if bpf.exhausted() {
                prepare_next_frame.assets.push((id, extracted_asset));
                continue;
            }
            size
        } else {
            0
        };

        match A::prepare_asset(extracted_asset, id, &mut param) {
            Ok(prepared_asset) => {
                render_assets.insert(id, prepared_asset);
                bpf.write_bytes(write_bytes);
                wrote_asset_count += 1;
            }
            Err(PrepareAssetError::RetryNextUpdate(extracted_asset)) => {
                prepare_next_frame.assets.push((id, extracted_asset));
            }
            Err(PrepareAssetError::AsBindGroupError(e)) => {
                error!(
                    "{} Bind group construction failed: {e}",
                    core::any::type_name::<A>()
                );
            }
        }
    }

    if bpf.exhausted() && !prepare_next_frame.assets.is_empty() {
        debug!(
            "{} write budget exhausted with {} assets remaining (wrote {})",
            core::any::type_name::<A>(),
            prepare_next_frame.assets.len(),
            wrote_asset_count
        );
    }
}

#[cfg(test)]
#[path = "erased_render_asset_tests.rs"]
mod revision_tests;
