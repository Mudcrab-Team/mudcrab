//! Pre-queue stock payload/pipeline proof. This does NOT prove phase membership.
//!
//! Run after stock specialization and before QueueMeshes. The caller must prove
//! current view visibility/render-layer eligibility, phase existence and later
//! actual phase membership separately. Mesh bind-group/uniform preparation also
//! occurs later; this helper does not certify those future draw commands.

use bevy::{
    ecs::system::SystemParam,
    material::{OpaqueRendererMethod, RenderPhaseType},
    pbr::{
        MainPassAlphaMaskDrawFunction, MainPassOpaqueDrawFunction,
        MainPassTransmissiveDrawFunction, MainPassTransparentDrawFunction,
        MaterialBindGroupAllocators, PreparedMaterial, RenderMaterialInstances,
        RenderMeshInstanceFlags, RenderMeshInstances, SpecializedMaterialPipelineCache,
        SpecializedPrepassMaterialPipelineCache, SpecializedShadowMaterialPipelineCache,
    },
    prelude::{AlphaMode, Res},
    render::{
        erased_render_asset::ErasedRenderAssets,
        mesh::{RenderMesh, allocator::MeshAllocator},
        render_asset::RenderAssets,
        render_resource::{CachedRenderPipelineId, PipelineCache},
        sync_world::MainEntity,
        view::RetainedViewEntity,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StockPassFamily {
    Forward,
    Prepass,
    Shadow,
}

/// Reads only current stock render resources; holds no cached certification.
#[derive(SystemParam)]
pub(crate) struct StockDrawable<'w> {
    mesh_instances: Option<Res<'w, RenderMeshInstances>>,
    material_instances: Option<Res<'w, RenderMaterialInstances>>,
    mesh_allocator: Res<'w, MeshAllocator>,
    meshes: Res<'w, RenderAssets<RenderMesh>>,
    materials: Res<'w, ErasedRenderAssets<PreparedMaterial>>,
    bind_groups: Option<Res<'w, MaterialBindGroupAllocators>>,
    pipelines: Res<'w, PipelineCache>,
    forward: Option<Res<'w, SpecializedMaterialPipelineCache>>,
    prepass: Option<Res<'w, SpecializedPrepassMaterialPipelineCache>>,
    shadow: Option<Res<'w, SpecializedShadowMaterialPipelineCache>>,
}

impl StockDrawable<'_> {
    /// Bind replacement proof to the immutable mesh extracted for this frame,
    /// rather than a prior entity instance with the same coverage identity.
    pub(crate) fn matches_mesh(
        &self,
        main: MainEntity,
        mesh: bevy::asset::AssetId<bevy::prelude::Mesh>,
    ) -> bool {
        self.mesh_instances
            .as_ref()
            .and_then(|instances| instances.mesh_asset_id(main))
            == Some(mesh)
    }

    /// A missing required pipeline/resource fails closed. A material with no
    /// applicable prepass/shadow pass does not require that pass's pipeline.
    /// `true` is pre-queue evidence only, never a promise that stock drew.
    pub(crate) fn drawable(
        &self,
        main: MainEntity,
        view: RetainedViewEntity,
        family: StockPassFamily,
    ) -> bool {
        let Some(instances) = self.mesh_instances.as_ref() else {
            return false;
        };
        let Some(mesh_id) = instances.mesh_asset_id(main) else {
            return false;
        };
        let Some(queue_data) = instances.render_mesh_queue_data(main) else {
            return false;
        };
        if queue_data.mesh_asset_id() != mesh_id
            || self.meshes.get(mesh_id).is_none()
            || self.mesh_allocator.mesh_slabs(&mesh_id).is_none()
        {
            return false;
        }
        let Some(instance) = self
            .material_instances
            .as_ref()
            .and_then(|instances| instances.instances.get(&main))
        else {
            return false;
        };
        let Some(material) = self.materials.get(instance.asset_id) else {
            return false;
        };
        // The slab must exist; its bind group need not yet. This proof runs before
        // `PrepareBindGroups`, which gives every slab its group before Render, and a slab
        // that any material re-prepares every frame (an animated one) never has it here.
        if self
            .bind_groups
            .as_ref()
            .and_then(|allocators| allocators.get(&instance.asset_id.type_id()))
            .and_then(|allocator| allocator.get(material.binding.group))
            .is_none()
        {
            return false;
        }
        match family {
            StockPassFamily::Forward => {
                // Mirror the forward queue's draw-function lookup. In particular,
                // an opaque deferred-only material is not forward-drawable.
                let draw = match material.properties.render_phase_type {
                    RenderPhaseType::Opaque => {
                        if material.properties.render_method == OpaqueRendererMethod::Deferred {
                            return false;
                        }
                        material
                            .properties
                            .get_draw_function(MainPassOpaqueDrawFunction)
                    }
                    RenderPhaseType::AlphaMask => material
                        .properties
                        .get_draw_function(MainPassAlphaMaskDrawFunction),
                    RenderPhaseType::Transmissive => material
                        .properties
                        .get_draw_function(MainPassTransmissiveDrawFunction),
                    RenderPhaseType::Transparent => material
                        .properties
                        .get_draw_function(MainPassTransparentDrawFunction),
                };
                if draw.is_none()
                    || !self
                        .forward
                        .as_ref()
                        .and_then(|cache| cache.get(&view))
                        .and_then(|entities| entities.get(&main))
                        .is_some_and(|id| self.pipeline_ready(*id))
                {
                    return false;
                }
                true
            }
            StockPassFamily::Prepass => {
                needs_prepass(material)
                    && self
                        .prepass
                        .as_ref()
                        .and_then(|cache| cache.get(&view))
                        .and_then(|entities| entities.get(&main))
                        .is_some_and(|(_, id, _)| self.pipeline_ready(*id))
            }
            StockPassFamily::Shadow => {
                if !material.properties.shadows_enabled {
                    return true;
                }
                // The shadow queue checks this again even after specialization.
                if !queue_data
                    .flags()
                    .contains(RenderMeshInstanceFlags::SHADOW_CASTER)
                {
                    return false;
                }
                self.shadow
                    .as_ref()
                    .and_then(|cache| cache.get(&view))
                    .and_then(|entities| entities.get(&main))
                    .is_some_and(|(id, _)| self.pipeline_ready(*id))
            }
        }
    }

    fn pipeline_ready(&self, id: CachedRenderPipelineId) -> bool {
        self.pipelines.get_render_pipeline(id).is_some()
    }
}

/// Matches stock prepass specialization eligibility, including transmissive
/// materials that otherwise have an opaque alpha mode.
fn needs_prepass(material: &PreparedMaterial) -> bool {
    material.properties.prepass_enabled
        && !material.properties.reads_view_transmission_texture
        && matches!(
            material.properties.alpha_mode,
            AlphaMode::Opaque | AlphaMode::Mask(_) | AlphaMode::AlphaToCoverage
        )
}
