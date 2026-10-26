//! Exact directional shadow-map retention. Producers own invalidation; settled
//! checks compare view inputs and revisions, never walk the caster population.

use core::{any::TypeId, marker::PhantomData};
use std::sync::{Mutex, MutexGuard};

use bevy_asset::UntypedAssetId;
use bevy_diagnostic::FrameCount;
use bevy_ecs::{
    prelude::*,
    query::ROQueryItem,
    system::{lifetimeless::SRes, SystemParamItem},
};
use bevy_platform::collections::{HashMap, HashSet};
use bevy_render::{
    erased_render_asset::ErasedRenderAssets,
    mesh::{allocator::MeshAllocator, RenderMesh},
    render_asset::RenderAssets,
    render_phase::{
        DrawFunctions, RenderCommand, RenderCommandResult, RenderCommandState, TrackedRenderPass,
    },
    render_resource::{Extent3d, PipelineCache, TextureId},
    sync_world::{MainEntity, MainEntityHashMap},
    texture::{CachedTexture, GpuImage},
    view::{ExtractedView, RetainedViewEntity},
};

use crate::{PreparedMaterial, RenderMeshInstances, Shadow, StandardMaterial};

fn lock<T: Default>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            // Never read partially published state. Leave poison set: every
            // later access discards state, and dependency snapshots refuse reuse
            // until the resource is recreated (e.g. device recovery).
            let mut state = poisoned.into_inner();
            *state = T::default();
            state
        }
    }
}

fn advance(revision: &mut u64) {
    *revision = revision.checked_add(1).expect("Shadow revision exhausted");
}

fn gpu_texture_contents_untracked(usage: bevy_render::render_resource::TextureUsages) -> bool {
    use bevy_render::render_resource::TextureUsages;
    usage.intersects(TextureUsages::RENDER_ATTACHMENT | TextureUsages::STORAGE_BINDING)
}

pub(crate) fn untracked_texture_bindings(
    bindings: &bevy_render::render_resource::BindingResources,
) -> bool {
    bindings.iter().any(|(_, resource)| match resource {
        bevy_render::render_resource::OwnedBindingResource::TextureView(_, view) => {
            gpu_texture_contents_untracked(view.texture().usage())
        }
        _ => false,
    })
}

#[derive(Clone, Copy)]
struct ExternalInput {
    frame: u32,
    revision: u64,
    complete: bool,
}

#[derive(Default)]
struct ExternalMaterials {
    frame: u32,
    initialized: bool,
    materials: HashSet<UntypedAssetId>,
    untracked: HashSet<UntypedAssetId>,
    unknown: HashSet<UntypedAssetId>,
    revisions: HashMap<UntypedAssetId, (u64, u64)>,
    revision: u64,
}

#[derive(Default)]
struct ExternalMaterialInputs {
    views: HashMap<RetainedViewEntity, ExternalMaterials>,
    material_views: HashMap<UntypedAssetId, HashSet<RetainedViewEntity>>,
    type_materials: HashMap<TypeId, HashSet<UntypedAssetId>>,
    pending: HashSet<UntypedAssetId>,
}

struct CertifiedShadowDraw {
    command: TypeId,
    views: HashMap<RetainedViewEntity, ExternalInput>,
}

#[derive(Default)]
struct ShadowDrawProducers {
    // Retained external command types registered by the application. Additional
    // producers conservatively refuse reuse when these slots are occupied.
    certified: [Option<CertifiedShadowDraw>; 2],
    declarations_revision: u64,
    registry_revision: Option<u64>,
    unknown: usize,
    active: [bool; 2],
    #[cfg(test)]
    classifications: u64,
}

type DrawProducerSnapshot = (u64, u64, u64, u64);

/// Current-frame dependency certificates for non-stock shadow producers.
///
/// Publish after culling, including frames that reuse the cull output. The
/// revision must cover the exact selected geometry, transforms, materials,
/// shader inputs, and resource replacements. `complete` also certifies that all
/// external draw pipelines and bindings are ready. Empty content needs an
/// explicit publication. Missing or old certificates refuse reuse.
#[derive(Resource, Default)]
pub struct ShadowMapExternalInputs(
    Mutex<HashMap<RetainedViewEntity, ExternalInput>>,
    Mutex<ExternalMaterialInputs>,
    Mutex<ShadowDrawProducers>,
);

impl ShadowMapExternalInputs {
    /// Certifies one external shadow command's complete dependencies for this
    /// view and frame. `C` is the inner command registered as
    /// `ShadowCacheTracking<C>`, not the wrapper itself.
    ///
    /// Publish after all of the producer's queue/input mutations, including
    /// insertion, removal, an empty output, and unchanged-output frames. The
    /// revision must cover every geometry, binding, uniform, shader, and resource
    /// input that can change this command's shadow output. Missing, incomplete,
    /// or stale certificates refuse reuse globally while this draw is registered.
    /// Wrapping or publishing the aggregate view certificate alone does not
    /// authorize a custom command. Raw `Draw` implementations cannot be certified.
    /// Up to two distinct retained command types registered by the application
    /// can be certified; any further registered producer refuses reuse.
    pub fn publish_draw<C: RenderCommand<Shadow> + 'static>(
        &self,
        view: RetainedViewEntity,
        frame: u32,
        revision: u64,
        complete: bool,
    ) {
        let command = TypeId::of::<RenderCommandState<Shadow, ShadowCacheTracking<C>>>();
        let mut producers = lock(&self.2);
        let slot = producers
            .certified
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|slot| slot.command == command))
            .or_else(|| producers.certified.iter().position(Option::is_none));
        let Some(slot) = slot else {
            // No declaration for extra types: the registry gate refuses them.
            return;
        };
        if producers.certified[slot].is_none() {
            producers.certified[slot] = Some(CertifiedShadowDraw {
                command,
                views: HashMap::default(),
            });
            advance(&mut producers.declarations_revision);
            producers.registry_revision = None;
        }
        if let Some(slot) = &mut producers.certified[slot] {
            slot.views.insert(
                view,
                ExternalInput {
                    frame,
                    revision,
                    complete,
                },
            );
        }
    }

    /// Marks freshness, retaining the previous plan's exact material closure.
    /// Invoke before the existing queue's unchanged-plan early return.
    pub fn begin_materials(&self, view: RetainedViewEntity, frame: u32) {
        lock(&self.1).views.entry(view).or_default().frame = frame;
    }

    /// Starts a new material closure only when the queue plan changes. Record
    /// its materials with `observe_material`, including an explicit empty plan.
    pub fn reset_materials(&self, view: RetainedViewEntity, frame: u32) {
        let mut inputs = lock(&self.1);
        if let Some(state) = inputs.views.get_mut(&view)
            && state.initialized
            && state.materials.is_empty()
        {
            // Empty publication is also the GPU-absent path. Refresh freshness
            // without changing exact content identity on every stock-only frame.
            state.frame = frame;
            return;
        }
        remove_external_view(&mut inputs, view);
        let state = inputs.views.entry(view).or_default();
        state.frame = frame;
        state.initialized = true;
        advance(&mut state.revision);
    }

    /// Records an already-fetched material in the existing queue loop. This is
    /// a constant-time flag check; it does not traverse textures or materials.
    pub fn observe_material(
        &self,
        view: RetainedViewEntity,
        frame: u32,
        id: UntypedAssetId,
        material: &PreparedMaterial,
    ) {
        let mut inputs = lock(&self.1);
        if let Some(state) = inputs
            .views
            .get_mut(&view)
            .filter(|inputs| inputs.frame == frame && inputs.initialized)
        {
            state.materials.insert(id);
            state.unknown.insert(id);
            if material.shadow_cache_untracked_texture {
                state.untracked.insert(id);
            }
            inputs.material_views.entry(id).or_default().insert(view);
            inputs
                .type_materials
                .entry(id.type_id())
                .or_default()
                .insert(id);
            inputs.pending.insert(id);
        }
    }

    /// Publishes the aggregate certificate for all non-stock producers of a view.
    pub fn publish(&self, view: RetainedViewEntity, frame: u32, revision: u64, complete: bool) {
        lock(&self.0).insert(
            view,
            ExternalInput {
                frame,
                revision,
                complete,
            },
        );
    }

    pub(crate) fn retain(&self, live: &HashSet<RetainedViewEntity>) {
        lock(&self.0).retain(|view, _| live.contains(view));
        for slot in lock(&self.2).certified.iter_mut().flatten() {
            slot.views.retain(|view, _| live.contains(view));
        }
        let mut materials = lock(&self.1);
        let expired: Vec<_> = materials
            .views
            .keys()
            .copied()
            .filter(|view| !live.contains(view))
            .collect();
        for view in expired {
            remove_external_view(&mut materials, view);
            materials.views.remove(&view);
        }
    }
}

fn remove_external_view(inputs: &mut ExternalMaterialInputs, view: RetainedViewEntity) {
    if let Some(state) = inputs.views.get_mut(&view) {
        for material in state.materials.drain() {
            if let Some(views) = inputs.material_views.get_mut(&material) {
                views.remove(&view);
                if views.is_empty() {
                    inputs.material_views.remove(&material);
                    if let Some(materials) = inputs.type_materials.get_mut(&material.type_id()) {
                        materials.remove(&material);
                    }
                }
            }
        }
        state.untracked.clear();
        state.unknown.clear();
        state.revisions.clear();
    }
}

fn route_external_material_change(
    inputs: &mut ExternalMaterialInputs,
    material: UntypedAssetId,
    revision: Option<(u64, u64)>,
    untracked: bool,
) {
    let ExternalMaterialInputs {
        views,
        material_views,
        ..
    } = inputs;
    let Some(affected) = material_views.get(&material) else {
        return;
    };
    for view in affected {
        let Some(state) = views.get_mut(view) else {
            continue;
        };
        if untracked {
            state.untracked.insert(material);
        } else {
            state.untracked.remove(&material);
        }
        if let Some(revision) = revision {
            if state.revisions.get(&material) != Some(&revision) {
                state.revisions.insert(material, revision);
                advance(&mut state.revision);
            }
            state.unknown.remove(&material);
        } else {
            state.revisions.remove(&material);
            state.unknown.insert(material);
        }
    }
}

/// Declared dependencies of custom stock shadow shaders.
///
/// `StandardMaterial` is known automatically. Custom shader types fail closed
/// until declared. Animated dependencies must publish through prepared-material
/// changes; declarations alone only certify static shader inputs.
#[derive(Resource)]
pub struct ShadowCacheMaterialDependencies {
    types: HashSet<TypeId>,
    changed_types: HashSet<TypeId>,
}

impl Default for ShadowCacheMaterialDependencies {
    fn default() -> Self {
        Self {
            types: [TypeId::of::<StandardMaterial>()].into_iter().collect(),
            changed_types: HashSet::default(),
        }
    }
}

impl ShadowCacheMaterialDependencies {
    /// Declares no shadow dependencies beyond the ordinary mesh, image,
    /// prepared-material, and view producers tracked by PBR.
    pub fn declare_static<M: 'static>(&mut self) {
        if self.types.insert(TypeId::of::<M>()) {
            self.changed_types.insert(TypeId::of::<M>());
        }
    }

    pub(crate) fn dependency(&self, material: UntypedAssetId) -> Option<u64> {
        self.types.contains(&material.type_id()).then_some(0)
    }
}

#[derive(Default)]
struct StockView {
    revision: u64,
    casters: MainEntityHashMap<UntypedAssetId>,
    materials: HashMap<UntypedAssetId, usize>,
    material_inputs: HashMap<UntypedAssetId, (u64, u64)>,
    material_revision: u64,
    unknown_materials: HashSet<UntypedAssetId>,
    untracked_textures: HashSet<UntypedAssetId>,
    incomplete: bool,
}

#[derive(Default)]
struct StockInputs {
    views: HashMap<RetainedViewEntity, StockView>,
    caster_views: MainEntityHashMap<HashSet<RetainedViewEntity>>,
    material_views: HashMap<UntypedAssetId, HashSet<RetainedViewEntity>>,
    type_materials: HashMap<TypeId, HashSet<UntypedAssetId>>,
    morph_weights: MainEntityHashMap<Vec<u32>>,
}

/// Producer-owned stock caster revisions, updated in existing extraction/queue
/// delta loops. The reverse index visits only views affected by a mutation.
#[derive(Resource, Default)]
#[doc(hidden)]
pub struct ShadowStockInputs(Mutex<StockInputs>, #[cfg(test)] StockInputOperations);

// Test-only work counters distinguish actual lock/index operations from elapsed
// CPU diagnostics. They add no state or instructions to production builds.
#[cfg(test)]
#[derive(Default)]
struct StockInputOperations {
    mutation_locks: core::sync::atomic::AtomicU64,
    caster_memberships: core::sync::atomic::AtomicU64,
}

impl ShadowStockInputs {
    fn lock_inputs(&self) -> MutexGuard<'_, StockInputs> {
        #[cfg(test)]
        self.1
            .mutation_locks
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        lock(&self.0)
    }

    /// Point/spot views never retain maps, so do not enter their lock/index
    /// bookkeeping at all. Eligibility is the same component used by rendering.
    pub(crate) fn for_directional_view(
        &self,
        inputs: Option<&DirectionalShadowMapInputs>,
    ) -> Option<&Self> {
        inputs.map(|_| self)
    }

    pub(crate) fn caster_changed(&self, entity: MainEntity) {
        let mut inputs = self.lock_inputs();
        let StockInputs {
            views,
            caster_views,
            ..
        } = &mut *inputs;
        if let Some(affected) = caster_views.get(&entity) {
            for view in affected {
                if let Some(view) = views.get_mut(view) {
                    advance(&mut view.revision);
                }
            }
        }
    }

    pub(crate) fn set_caster(
        &self,
        view: RetainedViewEntity,
        entity: MainEntity,
        material: UntypedAssetId,
        prepared_revision: Option<u64>,
        extra_revision: Option<u64>,
        untracked_texture: bool,
    ) {
        let mut inputs = self.lock_inputs();
        #[cfg(test)]
        self.1
            .caster_memberships
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let stock = inputs.views.entry(view).or_default();
        if stock.casters.get(&entity) == Some(&material) {
            // Requeue can change pipeline, geometry, layers, or uniform index.
            advance(&mut stock.revision);
            return;
        }
        let old_material = stock.casters.insert(entity, material);
        let mut remove_old_material = None;
        if let Some(old_material) = old_material
            && let Some(count) = stock.materials.get_mut(&old_material)
        {
            *count -= 1;
            if *count == 0 {
                stock.materials.remove(&old_material);
                stock.material_inputs.remove(&old_material);
                stock.unknown_materials.remove(&old_material);
                stock.untracked_textures.remove(&old_material);
                remove_old_material = Some(old_material);
            }
        }
        *stock.materials.entry(material).or_default() += 1;
        update_material(
            stock,
            material,
            prepared_revision.zip(extra_revision),
            untracked_texture,
        );
        advance(&mut stock.revision);
        if let Some(old_material) = remove_old_material {
            remove_material_view(&mut inputs, old_material, view);
        }
        inputs.caster_views.entry(entity).or_default().insert(view);
        inputs
            .material_views
            .entry(material)
            .or_default()
            .insert(view);
        inputs
            .type_materials
            .entry(material.type_id())
            .or_default()
            .insert(material);
    }

    pub(crate) fn remove_caster(&self, view: RetainedViewEntity, entity: MainEntity) {
        let mut inputs = self.lock_inputs();
        #[cfg(test)]
        self.1
            .caster_memberships
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let Some(stock) = inputs.views.get_mut(&view) else {
            return;
        };
        let Some(material) = stock.casters.remove(&entity) else {
            return;
        };
        let mut remove_material = false;
        if let Some(count) = stock.materials.get_mut(&material) {
            *count -= 1;
            if *count == 0 {
                stock.materials.remove(&material);
                stock.material_inputs.remove(&material);
                stock.unknown_materials.remove(&material);
                stock.untracked_textures.remove(&material);
                remove_material = true;
            }
        }
        advance(&mut stock.revision);
        if remove_material {
            remove_material_view(&mut inputs, material, view);
        }
        if let Some(views) = inputs.caster_views.get_mut(&entity) {
            views.remove(&view);
            if views.is_empty() {
                inputs.caster_views.remove(&entity);
            }
        }
    }

    pub(crate) fn observe_morph(&self, entity: MainEntity, weights: &[f32], source_changed: bool) {
        let changed = {
            let mut inputs = self.lock_inputs();
            // The extraction producer owns change ticks. Settled inputs need
            // only a membership lookup, never another walk over weight bytes.
            if !source_changed && inputs.morph_weights.contains_key(&entity) {
                return;
            }
            let old = inputs.morph_weights.entry(entity).or_default();
            if old
                .iter()
                .copied()
                .eq(weights.iter().map(|weight| weight.to_bits()))
            {
                false
            } else {
                old.clear();
                old.extend(weights.iter().map(|weight| weight.to_bits()));
                true
            }
        };
        if changed {
            self.caster_changed(entity);
        }
    }

    pub(crate) fn remove_morph(&self, entity: MainEntity) {
        let removed = self.lock_inputs().morph_weights.remove(&entity).is_some();
        if removed {
            self.caster_changed(entity);
        }
    }

    pub(crate) fn retain(&self, live: &HashSet<RetainedViewEntity>) {
        let mut inputs = self.lock_inputs();
        let StockInputs {
            views,
            caster_views,
            material_views,
            type_materials,
            ..
        } = &mut *inputs;
        views.retain(|view, stock| {
            if live.contains(view) {
                return true;
            }
            for entity in stock.casters.keys() {
                if let Some(affected) = caster_views.get_mut(entity) {
                    affected.remove(view);
                    if affected.is_empty() {
                        caster_views.remove(entity);
                    }
                }
            }
            for material in stock.materials.keys() {
                if let Some(affected) = material_views.get_mut(material) {
                    affected.remove(view);
                    if affected.is_empty() {
                        material_views.remove(material);
                        if let Some(materials) = type_materials.get_mut(&material.type_id()) {
                            materials.remove(material);
                        }
                    }
                }
            }
            false
        });
    }

    pub(crate) fn begin_view(&self, view: RetainedViewEntity) {
        self.lock_inputs().views.entry(view).or_default().incomplete = false;
    }

    pub(crate) fn refuse_view(&self, view: RetainedViewEntity) {
        self.lock_inputs().views.entry(view).or_default().incomplete = true;
    }

    fn snapshot(&self, view: RetainedViewEntity) -> Result<(u64, u64), Refusal> {
        let inputs = self.0.lock().map_err(|_| Refusal::PendingStock)?;
        let Some(stock) = inputs.views.get(&view) else {
            return Ok((0, 0));
        };
        if stock.incomplete {
            return Err(Refusal::PendingStock);
        }
        if !stock.unknown_materials.is_empty() {
            return Err(Refusal::UnknownMaterial);
        }
        if !stock.untracked_textures.is_empty() {
            return Err(Refusal::UntrackedTexture);
        }
        Ok((stock.revision, stock.material_revision))
    }
}

fn remove_material_view(
    inputs: &mut StockInputs,
    material: UntypedAssetId,
    view: RetainedViewEntity,
) {
    if let Some(views) = inputs.material_views.get_mut(&material) {
        views.remove(&view);
        if views.is_empty() {
            inputs.material_views.remove(&material);
            if let Some(materials) = inputs.type_materials.get_mut(&material.type_id()) {
                materials.remove(&material);
            }
        }
    }
}

fn update_material(
    stock: &mut StockView,
    material: UntypedAssetId,
    revision: Option<(u64, u64)>,
    untracked_texture: bool,
) {
    if untracked_texture {
        stock.untracked_textures.insert(material);
    } else {
        stock.untracked_textures.remove(&material);
    }
    if let Some(revision) = revision {
        if stock.material_inputs.get(&material) != Some(&revision) {
            stock.material_inputs.insert(material, revision);
            advance(&mut stock.material_revision);
        }
        stock.unknown_materials.remove(&material);
    } else {
        stock.material_inputs.remove(&material);
        stock.unknown_materials.insert(material);
    }
}

fn route_material_change(
    inputs: &mut StockInputs,
    material: UntypedAssetId,
    revision: Option<(u64, u64)>,
    untracked_texture: bool,
) {
    let StockInputs {
        views,
        material_views,
        ..
    } = inputs;
    let Some(affected) = material_views.get(&material) else {
        return;
    };
    for view in affected {
        if let Some(view) = views.get_mut(view) {
            update_material(view, material, revision, untracked_texture);
        }
    }
}

/// Routes producer mutation IDs only to cascades that use those materials.
/// There is no walk over unchanged materials or caster entities.
pub(crate) fn apply_shadow_material_changes(
    mut materials: ResMut<ErasedRenderAssets<PreparedMaterial>>,
    mut declarations: ResMut<ShadowCacheMaterialDependencies>,
    stock_inputs: Res<ShadowStockInputs>,
    external_inputs: Res<ShadowMapExternalInputs>,
) {
    let mut changed = materials.take_changed_assets();
    let mut inputs = lock(&stock_inputs.0);
    let mut external = lock(&external_inputs.1);
    changed.extend(external.pending.drain());
    for material_type in declarations.changed_types.drain() {
        if let Some(materials) = inputs.type_materials.get(&material_type) {
            changed.extend(materials.iter().copied());
        }
        if let Some(materials) = external.type_materials.get(&material_type) {
            changed.extend(materials.iter().copied());
        }
    }
    for material in changed {
        let revision = materials
            .asset_revision(material)
            .zip(declarations.dependency(material));
        let untracked_texture = materials
            .get(material)
            .is_none_or(|material| material.shadow_cache_untracked_texture);
        route_material_change(&mut inputs, material, revision, untracked_texture);
        route_external_material_change(&mut external, material, revision, untracked_texture);
    }
}

/// Persistent physical directional/spot array, reset on device recovery.
#[derive(Resource, Default)]
#[doc(hidden)]
pub struct RetainedDirectionalShadowTexture {
    pub(crate) texture: Option<(Extent3d, CachedTexture)>,
    // Extra camera atlases exist only when disjoint ranges exceed the device limit.
    pub(crate) camera_textures: bevy_ecs::entity::EntityHashMap<CachedTexture>,
    pub(crate) camera_slices: Vec<(Entity, core::ops::Range<usize>)>,
    pub(crate) spot_start: usize,
    pub(crate) overflow: bool,
}

/// Exact view/light/allocation inputs recorded by `prepare_lights`.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct DirectionalShadowMapInputs {
    pub(crate) texture: TextureId,
    pub(crate) slice: u32,
    pub(crate) matrices: [[u32; 16]; 7],
    pub(crate) light: [u32; 9],
    pub(crate) camera_layers: bevy_camera::visibility::RenderLayers,
    pub(crate) light_layers: bevy_camera::visibility::RenderLayers,
}

/// Exact uniforms of the shadow view, including renderer-extension changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ShadowViewInputs {
    viewport: [u32; 4],
    world_from_view: [u32; 16],
    clip_from_view: [u32; 16],
    clip_from_world: [u32; 16],
    mip_bias: u32,
    lod_origin: [u32; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MapKey {
    entity: Entity,
    inputs: DirectionalShadowMapInputs,
    shadow_view: ShadowViewInputs,
    external: u64,
    external_materials: u64,
    stock: (u64, u64),
    // These producer counters intentionally invalidate all maps. Narrowing
    // requires a complete per-caster dependency index; a global mutation is a
    // conservative miss, never evidence that an unrelated map is unchanged.
    geometry: u64,
    meshes: u64,
    images: u64,
    pipelines: u64,
    draw_producers: DrawProducerSnapshot,
}

// Reclassify only on a registry/declaration mutation. Draw storage is read-only
// outside its mutation owner, so this stamp covers insertion, replacement,
// removal, and resource recreation. Actual types cannot be spoofed by `add_with`
// aliases. Settled checks read a cached unknown count and at most two current
// certificates: no registry/phase scan, type checks, or allocation.
fn shadow_draw_producers(
    draws: &DrawFunctions<Shadow>,
    external: &ShadowMapExternalInputs,
    view: RetainedViewEntity,
    frame: u32,
) -> Result<DrawProducerSnapshot, Refusal> {
    let draws = draws.read();
    let registry_revision = draws.draw_functions.revision();
    let mut producers = external
        .2
        .lock()
        .map_err(|_| Refusal::UnsupportedProducer)?;
    if producers.registry_revision != Some(registry_revision) {
        let stock = [
            TypeId::of::<RenderCommandState<Shadow, ShadowCacheTracking<crate::DrawPrepass>>>(),
            TypeId::of::<
                RenderCommandState<Shadow, ShadowCacheTracking<crate::DrawDepthOnlyPrepass>>,
            >(),
        ];
        let mut unknown = 0;
        let mut active = [false; 2];
        for draw in draws.draw_functions.iter() {
            let command = core::any::Any::type_id(draw.as_ref());
            if stock.contains(&command) {
                continue;
            }
            if let Some(slot) = producers
                .certified
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|slot| slot.command == command))
            {
                active[slot] = true;
            } else {
                unknown += 1;
            }
        }
        producers.unknown = unknown;
        producers.active = active;
        producers.registry_revision = Some(registry_revision);
        #[cfg(test)]
        {
            producers.classifications += 1;
        }
    }
    if producers.unknown != 0 {
        return Err(Refusal::UnsupportedProducer);
    }
    let mut revisions = [0; 2];
    for (slot, revision) in revisions.iter_mut().enumerate() {
        if !producers.active[slot] {
            continue;
        }
        let input = producers.certified[slot]
            .as_ref()
            .and_then(|producer| producer.views.get(&view))
            .filter(|input| input.frame == frame && input.complete)
            .ok_or(Refusal::UnsupportedProducer)?;
        *revision = input.revision;
    }
    Ok((
        registry_revision,
        producers.declarations_revision,
        revisions[0],
        revisions[1],
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    External,
    UnknownMaterial,
    UnsupportedProducer,
    PendingStock,
    UntrackedTexture,
}

/// Monotonic counters. Reuse skips clear and draw; refusals are separate from
/// ordinary dependency misses and incomplete-command retries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShadowMapCacheCounters {
    pub reused: u64,
    pub rendered: u64,
    pub invalidated: u64,
    pub external_incomplete: u64,
    pub unknown_material: u64,
    pub unsupported_producer: u64,
    pub stock_pending: u64,
    pub incomplete_draw: u64,
    pub untracked_texture: u64,
}

impl ShadowMapCacheCounters {
    fn begin(&mut self, key: &Result<MapKey, Refusal>, skip: bool) {
        match key {
            Ok(_) if skip => self.reused += 1,
            Ok(_) => self.invalidated += 1,
            Err(Refusal::External) => self.external_incomplete += 1,
            Err(Refusal::UnknownMaterial) => self.unknown_material += 1,
            Err(Refusal::UnsupportedProducer) => self.unsupported_producer += 1,
            Err(Refusal::PendingStock) => self.stock_pending += 1,
            Err(Refusal::UntrackedTexture) => self.untracked_texture += 1,
        }
        if !skip {
            self.rendered += 1;
        }
    }
}

/// Scalar deltas owned by the retained attempt, not by a caster or dependency
/// scan. `rendered` counts view/frame render decisions, including a late pass
/// that revokes early reuse; it is not a count of early and late GPU passes.
struct ShadowViewWindow {
    pass_name: String,
    counters: ShadowMapCacheCounters,
    start_frame: u32,
    last_observed_frame: Option<u32>,
    observed_frames: u32,
    label_changes: u64,
    label_valid: bool,
}

#[derive(Clone, Copy)]
struct ShadowViewWindowReport {
    counters: ShadowMapCacheCounters,
    start_frame: u32,
    end_frame: u32,
    frames: u32,
    observed_frames: u32,
    label_changes: u64,
    label_valid: bool,
    lifecycle_valid: bool,
}

impl ShadowViewWindowReport {
    fn zero_render_proof(&self) -> bool {
        let c = &self.counters;
        self.label_valid
            && self.lifecycle_valid
            && self.observed_frames != 0
            && self.observed_frames == self.frames
            && c.reused == u64::from(self.observed_frames)
            && c.rendered == 0
            && c.invalidated == 0
            && c.external_incomplete == 0
            && c.unknown_material == 0
            && c.unsupported_producer == 0
            && c.stock_pending == 0
            && c.untracked_texture == 0
            && c.incomplete_draw == 0
    }
}

impl ShadowViewWindow {
    fn has_pending(&self) -> bool {
        self.observed_frames != 0 || self.counters != ShadowMapCacheCounters::default()
    }

    fn new(frame: u32) -> Self {
        Self {
            pass_name: String::new(),
            counters: ShadowMapCacheCounters::default(),
            start_frame: frame,
            last_observed_frame: None,
            observed_frames: 0,
            label_changes: 0,
            label_valid: true,
        }
    }

    fn observe(&mut self, frame: u32, pass_name: &str) {
        if self.last_observed_frame != Some(frame) {
            self.observed_frames += 1;
            self.last_observed_frame = Some(frame);
        }
        if self.pass_name != pass_name {
            if !self.pass_name.is_empty() {
                self.label_changes += 1;
                self.label_valid = false;
            }
            // Retain storage between windows. No label allocation or copy on
            // an unchanged view, and no temporary String in the render path.
            self.pass_name.clear();
            self.pass_name.push_str(pass_name);
        }
        self.label_valid &= !pass_name.is_empty();
    }

    fn record(&mut self, update: impl FnOnce(&mut ShadowMapCacheCounters)) {
        update(&mut self.counters);
    }

    fn close(&mut self, frame: u32, lifecycle_valid: bool) -> ShadowViewWindowReport {
        let report = ShadowViewWindowReport {
            counters: core::mem::take(&mut self.counters),
            start_frame: self.start_frame,
            end_frame: frame,
            frames: frame.wrapping_sub(self.start_frame).wrapping_add(1),
            observed_frames: self.observed_frames,
            label_changes: self.label_changes,
            label_valid: self.label_valid,
            lifecycle_valid,
        };
        self.start_frame = frame.wrapping_add(1);
        self.observed_frames = 0;
        self.label_changes = 0;
        self.label_valid = !self.pass_name.is_empty();
        report
    }

    fn report(&mut self, view: RetainedViewEntity, frame: u32, reason: &'static str) {
        let report = self.close(frame, reason == "periodic");
        let c = report.counters;
        tracing::info!(
            main_entity = view.main_entity.id().to_bits(),
            auxiliary_entity = view.auxiliary_entity.id().to_bits(),
            subview_index = view.subview_index,
            pass_name = self.pass_name.as_str(),
            window_start_frame = report.start_frame,
            window_end_frame = report.end_frame,
            frames = report.frames,
            observed_frames = report.observed_frames,
            label_changes = report.label_changes,
            label_valid = report.label_valid,
            lifecycle_valid = report.lifecycle_valid,
            zero_render_proof = report.zero_render_proof(),
            reason,
            reused = c.reused,
            rendered = c.rendered,
            invalidated = c.invalidated,
            external_incomplete = c.external_incomplete,
            unknown_material = c.unknown_material,
            unsupported_producer = c.unsupported_producer,
            stock_pending = c.stock_pending,
            incomplete_draw = c.incomplete_draw,
            untracked_texture = c.untracked_texture,
            "shadow map per-view reuse window"
        );
    }
}

struct Attempt {
    frame: u32,
    skip: bool,
    failed: bool,
    early_complete: bool,
    key: Option<MapKey>,
    window: ShadowViewWindow,
}

#[derive(Default)]
struct Maps {
    retention_disabled: bool,
    owners: HashMap<(TextureId, u32), (RetainedViewEntity, MapKey)>,
    attempts: HashMap<RetainedViewEntity, Attempt>,
    counters: ShadowMapCacheCounters,
}

/// Retained maps and their physical last-writer certificates.
#[derive(Resource)]
pub struct ShadowMapCache(Mutex<Maps>);

impl FromWorld for ShadowMapCache {
    fn from_world(_world: &mut World) -> Self {
        Self(Mutex::default())
    }
}

impl ShadowMapCache {
    fn maps(&self) -> MutexGuard<'_, Maps> {
        match self.0.lock() {
            Ok(maps) => maps,
            Err(poisoned) => {
                let mut maps = poisoned.into_inner();
                // Disable retention for this resource's remaining lifetime. Keep
                // only the per-frame early-skip decision: late must still skip
                // if early reused, but a fresh early pass must receive its real
                // late occlusion work. Rust collections remain memory-safe after
                // unwinding; no partially published key can certify an owner.
                maps.retention_disabled = true;
                maps.owners.clear();
                for attempt in maps.attempts.values_mut() {
                    attempt.failed = true;
                    attempt.key = None;
                    attempt.window.label_valid = false;
                }
                self.0.clear_poison();
                maps
            }
        }
    }

    /// Reads cumulative counters without scanning maps or casters.
    pub fn counters(&self) -> ShadowMapCacheCounters {
        self.maps().counters
    }

    pub(crate) fn invalidate_physical_maps(&self) {
        let mut maps = self.maps();
        maps.owners.clear();
        for (view, attempt) in &mut maps.attempts {
            if attempt.window.has_pending() {
                attempt
                    .window
                    .report(*view, attempt.frame, "physical_reset");
            }
        }
        maps.attempts.clear();
    }

    pub(crate) fn retain(&self, live: &HashSet<RetainedViewEntity>) {
        let mut maps = self.maps();
        maps.owners.retain(|_, (view, _)| live.contains(view));
        maps.attempts.retain(|view, attempt| {
            if live.contains(view) {
                return true;
            }
            if attempt.window.has_pending() {
                attempt.window.report(*view, attempt.frame, "view_removed");
            }
            false
        });
    }

    pub(crate) fn begin(
        &self,
        world: &World,
        entity: Entity,
        view: RetainedViewEntity,
        late: bool,
        pass_name: &str,
    ) -> bool {
        let frame = world.resource::<FrameCount>().0;
        let producers = shadow_draw_producers(
            world.resource::<DrawFunctions<Shadow>>(),
            world.resource::<ShadowMapExternalInputs>(),
            view,
            frame,
        );
        if late {
            self.validate_draw_producers(view, producers);
            return self
                .maps()
                .attempts
                .get(&view)
                .is_some_and(|attempt| attempt.frame == frame && attempt.skip);
        }
        let key = make_key(world, entity, view, frame);
        let allocation = world
            .get::<DirectionalShadowMapInputs>(entity)
            .map(|inputs| (inputs.texture, inputs.slice));

        self.begin_key(view, frame, key, allocation, producers, pass_name)
    }

    fn begin_key(
        &self,
        view: RetainedViewEntity,
        frame: u32,
        key: Result<MapKey, Refusal>,
        allocation: Option<(TextureId, u32)>,
        producers: Result<DrawProducerSnapshot, Refusal>,
        pass_name: &str,
    ) -> bool {
        let key = key.and_then(|mut key| {
            key.draw_producers = producers?;
            Ok(key)
        });
        let mut maps = self.maps();
        let key = if maps.retention_disabled {
            Err(Refusal::UnsupportedProducer)
        } else {
            key
        };
        let skip = key.as_ref().is_ok_and(|key| {
            maps.owners
                .get(&(key.inputs.texture, key.inputs.slice))
                .is_some_and(|(owner, previous)| *owner == view && previous == key)
        });
        maps.counters.begin(&key, skip);
        if !skip {
            // Clear invalidates the old writer even when the new phase skips.
            if let Some(allocation) = allocation {
                maps.owners.remove(&allocation);
            }
        }
        let attempt = maps.attempts.entry(view).or_insert_with(|| Attempt {
            frame,
            skip,
            failed: false,
            early_complete: false,
            key: None,
            window: ShadowViewWindow::new(frame),
        });
        attempt.window.observe(frame, pass_name);
        attempt.window.record(|c| c.begin(&key, skip));
        attempt.frame = frame;
        attempt.skip = skip;
        attempt.failed = false;
        attempt.early_complete = false;
        attempt.key = key.ok();
        skip
    }

    fn incomplete(&self, view: RetainedViewEntity) {
        let mut maps = self.maps();
        if let Some(attempt) = maps.attempts.get_mut(&view) {
            attempt.failed = true;
            attempt.window.record(|c| c.incomplete_draw += 1);
            maps.counters.incomplete_draw += 1;
        }
    }

    fn command_result(&self, view: RetainedViewEntity, result: &RenderCommandResult) {
        if !matches!(result, RenderCommandResult::Success) {
            self.incomplete(view);
        }
    }

    fn validate_draw_producers(
        &self,
        view: RetainedViewEntity,
        producers: Result<DrawProducerSnapshot, Refusal>,
    ) -> bool {
        let mut maps = self.maps();
        let Some(attempt) = maps.attempts.get_mut(&view) else {
            return false;
        };
        let Some(key) = &attempt.key else {
            return false;
        };
        if producers
            .as_ref()
            .is_ok_and(|producers| *producers == key.draw_producers)
        {
            return true;
        }
        let allocation = (key.inputs.texture, key.inputs.slice);
        attempt.window.record(|c| {
            if producers.is_err() {
                c.unsupported_producer += 1;
            } else {
                c.invalidated += 1;
            }
        });
        attempt.failed = true;
        // If early rendering was skipped, the attachment still contains the
        // retained full map. Skip late drawing too; loading then appending only
        // late-visible casters would produce an old/new mixture. The removed
        // owner forces a cleared full render on the next early pass.
        maps.owners.remove(&allocation);
        if producers.is_err() {
            maps.counters.unsupported_producer += 1;
        } else {
            maps.counters.invalidated += 1;
        }
        false
    }

    pub(crate) fn finish_with_world(
        &self,
        world: &World,
        view: RetainedViewEntity,
        late: bool,
        occlusion: bool,
        complete: bool,
    ) {
        let producers = shadow_draw_producers(
            world.resource::<DrawFunctions<Shadow>>(),
            world.resource::<ShadowMapExternalInputs>(),
            view,
            world.resource::<FrameCount>().0,
        );
        self.validate_draw_producers(view, producers);
        self.finish(view, late, occlusion, complete);
    }

    pub(crate) fn finish(
        &self,
        view: RetainedViewEntity,
        late: bool,
        occlusion: bool,
        complete: bool,
    ) {
        let mut maps = self.maps();
        let Some(attempt) = maps.attempts.get_mut(&view) else {
            return;
        };
        attempt.failed |= !complete;
        if !complete {
            attempt.window.record(|c| c.incomplete_draw += 1);
        }
        if !late {
            attempt.early_complete = complete;
        }
        if attempt.skip || attempt.failed || !attempt.early_complete || (occlusion && !late) {
            return;
        }
        if let Some(key) = attempt.key.clone() {
            maps.owners
                .insert((key.inputs.texture, key.inputs.slice), (view, key));
        }
    }
}

pub(crate) fn report_shadow_cache_counters(
    cache: Res<ShadowMapCache>,
    frame: Res<FrameCount>,
    mut previous: Local<(u32, ShadowMapCacheCounters)>,
) {
    if frame.0.wrapping_sub(previous.0) < 300 {
        return;
    }
    let current = cache.counters();
    let old = previous.1;
    tracing::info!(
        frames = frame.0.wrapping_sub(previous.0),
        reused = current.reused.saturating_sub(old.reused),
        rendered = current.rendered.saturating_sub(old.rendered),
        invalidated = current.invalidated.saturating_sub(old.invalidated),
        external_incomplete = current
            .external_incomplete
            .saturating_sub(old.external_incomplete),
        unknown_material = current
            .unknown_material
            .saturating_sub(old.unknown_material),
        unsupported_producer = current
            .unsupported_producer
            .saturating_sub(old.unsupported_producer),
        stock_pending = current.stock_pending.saturating_sub(old.stock_pending),
        incomplete_draw = current.incomplete_draw.saturating_sub(old.incomplete_draw),
        untracked_texture = current
            .untracked_texture
            .saturating_sub(old.untracked_texture),
        "shadow map reuse window"
    );
    // Cleanup follows all early/late rendering. Closing a
    // window here cannot lose a late reuse-to-render correction. This walks
    // retained attempts only once per report, never caster/dependency data.
    for (view, attempt) in &mut cache.maps().attempts {
        attempt.window.report(*view, frame.0, "periodic");
    }
    *previous = (frame.0, current);
}

fn make_key(
    world: &World,
    entity: Entity,
    view: RetainedViewEntity,
    frame: u32,
) -> Result<MapKey, Refusal> {
    let external_inputs = world.resource::<ShadowMapExternalInputs>();
    let external = external_inputs
        .0
        .lock()
        .map_err(|_| Refusal::External)?
        .get(&view)
        .copied()
        .filter(|input| input.frame == frame && input.complete)
        .ok_or(Refusal::External)?;
    let external_materials = {
        let materials = external_inputs.1.lock().map_err(|_| Refusal::External)?;
        let inputs = materials
            .views
            .get(&view)
            .filter(|inputs| inputs.frame == frame && inputs.initialized)
            .ok_or(Refusal::External)?;
        if !inputs.untracked.is_empty() {
            return Err(Refusal::UntrackedTexture);
        }
        if !inputs.unknown.is_empty() {
            return Err(Refusal::UnknownMaterial);
        }
        inputs.revision
    };
    if !matches!(
        world.resource::<RenderMeshInstances>(),
        RenderMeshInstances::GpuBuilding(_)
    ) {
        return Err(Refusal::UnsupportedProducer);
    }
    if world
        .resource::<crate::PendingShadowQueues>()
        .get(&view)
        .is_some_and(|pending| !pending.current_frame.is_empty())
    {
        return Err(Refusal::PendingStock);
    }
    let stock = world.resource::<ShadowStockInputs>().snapshot(view)?;
    let inputs = world
        .get::<DirectionalShadowMapInputs>(entity)
        .cloned()
        .ok_or(Refusal::UnsupportedProducer)?;
    // Include actual shadow-view state too, in case a renderer extension sets
    // per-shadow uniforms after prepare_lights recorded the camera/light inputs.
    let shadow_view = world
        .get::<ExtractedView>(entity)
        .ok_or(Refusal::UnsupportedProducer)?;
    let shadow_view = ShadowViewInputs {
        viewport: shadow_view.viewport.to_array(),
        world_from_view: shadow_view
            .world_from_view
            .to_matrix()
            .to_cols_array()
            .map(f32::to_bits),
        clip_from_view: shadow_view.clip_from_view.to_cols_array().map(f32::to_bits),
        clip_from_world: shadow_view
            .clip_from_world
            .unwrap_or_else(|| {
                shadow_view.clip_from_view * shadow_view.world_from_view.to_matrix().inverse()
            })
            .to_cols_array()
            .map(f32::to_bits),
        mip_bias: world
            .get::<bevy_render::camera::MipBias>(entity)
            .map_or(0.0, |bias| bias.0)
            .to_bits(),
        lod_origin: world
            .resource::<bevy_render::view::RenderShadowLodOrigin>()
            .0
            .to_array()
            .map(f32::to_bits),
    };
    Ok(MapKey {
        entity,
        inputs,
        shadow_view,
        external: external.revision,
        external_materials,
        stock,
        geometry: world.resource::<MeshAllocator>().content_revision(),
        meshes: world.resource::<RenderAssets<RenderMesh>>().revision(),
        images: world.resource::<RenderAssets<GpuImage>>().revision(),
        pipelines: world.resource::<PipelineCache>().readiness_revision(),
        draw_producers: (0, 0, 0, 0),
    })
}

/// Wraps a shadow command tuple so `Skip` cannot certify a partially rendered
/// map. Register and look up this wrapped type in `DrawFunctions`.
pub struct ShadowCacheTracking<C>(PhantomData<C>);

impl<C: RenderCommand<Shadow>> RenderCommand<Shadow> for ShadowCacheTracking<C> {
    type Param = (C::Param, SRes<ShadowMapCache>);
    type ViewQuery = (
        C::ViewQuery,
        &'static ExtractedView,
        Has<DirectionalShadowMapInputs>,
    );
    type ItemQuery = C::ItemQuery;

    fn render<'w>(
        item: &Shadow,
        (view, extracted, cacheable): ROQueryItem<'w, '_, Self::ViewQuery>,
        entity: Option<ROQueryItem<'w, '_, Self::ItemQuery>>,
        (param, cache): SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let result = C::render(item, view, entity, param, pass);
        if cacheable {
            cache.command_result(extracted.retained_view_entity, &result);
        }
        result
    }
}

#[cfg(test)]
#[path = "shadow_cache_tests.rs"]
mod tests;
