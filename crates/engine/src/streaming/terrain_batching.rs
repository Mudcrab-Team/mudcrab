//! Runtime terrain submission batches. Source GLBs and their quadrant contract stay unchanged.
//!
//! Vertex streams are concatenated without welding or transforming them. A batch's index buffer
//! contains precisely the quadrants selected by the ordinary LOD handoff. The two-cell tile bounds
//! limit the loss of culling granularity; batches never cross a chunk, material, or render state.

use super::{TerrainCoverage, TerrainSurfaceReady};
use crate::terrain_upload::TerrainMeshUploadReadiness;
use crate::visibility_optimization::StructuralHierarchyNode;
use bevy::{
    animation::AnimatedBy,
    asset::RenderAssetUsages,
    camera::{
        primitives::{Aabb, Frustum, Sphere},
        visibility::{
            NoAutoAabb, NoCpuCulling, NoFrustumCulling, RenderLayers, VisibilityClass,
            VisibilityRange,
        },
    },
    ecs::{lifecycle::HookContext, system::SystemParam, world::DeferredWorld},
    gizmos::aabb::ShowAabbGizmo,
    light::{LightProbe, NotShadowCaster, NotShadowReceiver, RectLight, TransmittedShadowReceiver},
    mesh::{
        Indices, MeshTag,
        morph::{MeshMorphWeights, MorphWeights},
        skinning::SkinnedMesh,
    },
    pbr::Lightmap,
    picking::Pickable,
    prelude::*,
    render::batching::NoAutomaticBatching,
    world_serialization::{WorldAsset, WorldAssetRoot, WorldInstance},
};
use bevy_rapier3d::prelude::{Collider, RigidBody};
use shared::lod::{LodTier, nodes};
use std::{
    any::TypeId,
    collections::{HashMap, HashSet},
};

const BATCH_SIDE_CELLS: i32 = 2;

/// A render mesh representing at most sixteen source quadrants in a two-by-two-cell tile.
#[derive(Component)]
pub(crate) struct LodTerrainBatch;

#[derive(Component)]
pub(crate) struct PendingLodTerrainBatching {
    pub(super) source: Handle<WorldAsset>,
    pub(super) patches: Vec<(Entity, TerrainCoverage)>,
}

#[derive(Component)]
#[component(on_remove = remove_generated_meshes)]
pub(super) struct LodTerrainBatches {
    batches: Vec<TerrainBatch>,
    source_children: Option<Vec<Entity>>,
    // Removing WorldAssetRoot unregisters the original instance before its hierarchy is flattened.
    // Retain its handle so batching never mutates or unloads the source scene's shared mesh assets.
    _source: Handle<WorldAsset>,
}

fn remove_generated_meshes(mut world: DeferredWorld, context: HookContext) {
    let ids: Vec<_> = world
        .get::<LodTerrainBatches>(context.entity)
        .into_iter()
        .flat_map(|batches| batches.batches.iter().map(|batch| batch.mesh.id()))
        .collect();
    let source_ids: Vec<_> = world
        .get::<LodTerrainBatches>(context.entity)
        .into_iter()
        .flat_map(|chunk| {
            chunk
                .batches
                .iter()
                .flat_map(|batch| batch.members.iter().map(|member| member.source.id()))
        })
        .collect();
    if let Some(mut meshes) = world.get_resource_mut::<Assets<Mesh>>() {
        for &id in &ids {
            meshes.remove(id);
        }
    }
    if let Some(readiness) = world.get_resource::<TerrainMeshUploadReadiness>() {
        for id in ids {
            readiness.forget(id);
        }
        for id in source_ids {
            readiness.forget(id);
        }
    }
}

struct TerrainBatch {
    entity: Entity,
    mesh: Handle<Mesh>,
    members: Vec<BatchMember>,
    selected: u16,
    state: RenderState,
    pending_upload: bool,
    fallback_entities: Vec<Entity>,
}

struct BatchMember {
    source_patch: Entity,
    coverage: TerrainCoverage,
    indices: Vec<u32>,
    bounds: Aabb,
    source: Handle<Mesh>,
}

#[derive(Clone, PartialEq)]
struct RenderState {
    material: Handle<StandardMaterial>,
    transform: Transform,
    layers: Option<RenderLayers>,
    tag: Option<u32>,
    not_shadow_caster: bool,
    not_shadow_receiver: bool,
    transmitted_shadow_receiver: bool,
    no_frustum_culling: bool,
}

pub(super) struct PreparedBatch {
    tile: IVec2,
    state: RenderState,
    mesh: Mesh,
    members: Vec<BatchMember>,
    selected: u16,
}

type PrimitiveRenderState = (
    &'static MeshMaterial3d<StandardMaterial>,
    Option<&'static RenderLayers>,
    Option<&'static MeshTag>,
    Has<NotShadowCaster>,
    Has<NotShadowReceiver>,
    Has<TransmittedShadowReceiver>,
    Has<NoFrustumCulling>,
);

type PrimitiveCullingState = (
    Has<NoCpuCulling>,
    Has<StructuralHierarchyNode>,
    Option<&'static VisibilityClass>,
    Has<Aabb>,
);

type UnsupportedNode = Or<(
    Or<(
        With<SkinnedMesh>,
        With<MeshMorphWeights>,
        With<MorphWeights>,
        With<AnimationPlayer>,
        With<AnimatedBy>,
        With<Lightmap>,
        With<VisibilityRange>,
        With<NoAutomaticBatching>,
        With<Camera>,
        With<PointLight>,
        With<SpotLight>,
        With<DirectionalLight>,
    )>,
    Or<(
        With<RectLight>,
        With<LightProbe>,
        With<Collider>,
        With<RigidBody>,
        With<Pickable>,
        With<ShowAabbGizmo>,
        With<Frustum>,
        With<Sphere>,
        With<Mesh2d>,
    )>,
)>;

#[derive(SystemParam)]
pub(crate) struct LodBatchPreparation<'w, 's> {
    transforms: Query<'w, 's, (&'static Transform, Option<&'static Visibility>)>,
    layers: Query<'w, 's, &'static RenderLayers>,
    render_states: Query<'w, 's, PrimitiveRenderState>,
    unsupported: Query<'w, 's, Entity, UnsupportedNode>,
    culling: Query<'w, 's, PrimitiveCullingState>,
    materials: Res<'w, Assets<StandardMaterial>>,
    readiness: Option<Res<'w, TerrainMeshUploadReadiness>>,
}

impl LodBatchPreparation<'_, '_> {
    pub(super) fn upload_readiness(&self) -> Option<&TerrainMeshUploadReadiness> {
        self.readiness.as_deref()
    }

    pub(super) fn has_render_backend(&self) -> bool {
        self.readiness
            .as_deref()
            .is_some_and(TerrainMeshUploadReadiness::has_render_backend)
    }
    /// Refuse the whole conversion before changing anything when a scene has extra content or
    /// behavior that cannot be represented by the terrain-only submission contract.
    pub(super) fn prepare(
        &self,
        root: Entity,
        patches: &[(Entity, TerrainCoverage)],
        children: &Query<&Children>,
        names: &Query<&Name>,
        mesh_handles: &Query<&Mesh3d>,
        meshes: &Assets<Mesh>,
    ) -> Result<Vec<PreparedBatch>, String> {
        let mut primitives = HashMap::new();
        let patch_entities: HashSet<_> = patches.iter().map(|(entity, _)| *entity).collect();
        for &(patch, coverage) in patches {
            let primitive = children
                .get(patch)
                .ok()
                .and_then(|children| children.iter().find(|child| mesh_handles.contains(*child)))
                .ok_or("terrain primitive disappeared before batching")?;
            let selected = self
                .transforms
                .get(patch)
                .ok()
                .and_then(|(_, visibility)| visibility)
                .is_some_and(|visibility| *visibility == Visibility::Inherited);
            primitives.insert(primitive, (patch, coverage, selected));
        }

        let mut primitive_transforms = HashMap::new();
        let mut stack: Vec<(Entity, Transform, bool)> = children
            .get(root)
            .map(|children| {
                children
                    .iter()
                    .map(|child| (child, Transform::IDENTITY, true))
                    .collect()
            })
            .unwrap_or_default();
        while let Some((entity, parent_transform, scene_root)) = stack.pop() {
            if self.unsupported.contains(entity) {
                return Err(
                    "scene contains animation, skinning, lightmaps, or other render behavior"
                        .into(),
                );
            }
            let (transform, visibility) = self
                .transforms
                .get(entity)
                .map_err(|_| "scene node has no local transform")?;
            if !patch_entities.contains(&entity)
                && visibility.is_some_and(|visibility| *visibility != Visibility::Inherited)
            {
                return Err("scene node has explicit visibility".into());
            }
            // The compiler places all geometry in chunk-local coordinates and gives only its
            // scene root an axis transform. Keep that exact transform rather than baking it into
            // vertices or decomposing a composed matrix that might contain shear.
            let primitive = primitives.contains_key(&entity);
            let name = names.get(entity).ok().map(Name::as_str);
            let chunk_wrapper = !primitive && name.is_some_and(|name| name.starts_with("chunk_"));
            if *transform != Transform::IDENTITY
                && (!chunk_wrapper || parent_transform != Transform::IDENTITY)
            {
                return Err("terrain descendants have additional local transforms".into());
            }
            let combined = if *transform != Transform::IDENTITY {
                *transform
            } else {
                parent_transform
            };
            let (no_cpu, owned_structural, class, bounds) = self
                .culling
                .get(entity)
                .map_err(|_| "terrain culling state disappeared")?;
            if (no_cpu && (primitive || !owned_structural))
                || (primitive
                    && class.is_some_and(|class| {
                        class.0.iter().any(|id| *id != TypeId::of::<Mesh3d>())
                    }))
                || (!primitive && (class.is_some() || bounds))
            {
                return Err(
                    "scene contains explicit culling or additional visibility participants".into(),
                );
            }
            if primitive {
                if children
                    .get(entity)
                    .is_ok_and(|children| !children.is_empty())
                {
                    return Err("terrain primitive has extra descendants".into());
                }
                primitive_transforms.insert(entity, combined);
            } else {
                let name = names
                    .get(entity)
                    .map(Name::as_str)
                    .map_err(|_| "scene contains an unnamed non-terrain node")?;
                if self.layers.contains(entity)
                    || mesh_handles.contains(entity)
                    || !(name.starts_with("chunk_")
                        || name.starts_with("cell_")
                        || name == nodes::terrain_group()
                        || name.starts_with("terrain_quadrant_"))
                {
                    return Err(format!("scene contains non-terrain node {name:?}"));
                }
                if scene_root && !chunk_wrapper {
                    return Err("scene root does not match the compiler terrain hierarchy".into());
                }
                if let Ok(children) = children.get(entity) {
                    stack.extend(children.iter().map(|child| (child, combined, false)));
                }
            }
        }
        if primitive_transforms.len() != primitives.len() {
            return Err("terrain primitives lie outside their chunk hierarchy".into());
        }

        let mut ordered: Vec<_> = primitives.into_iter().collect();
        ordered.sort_by_key(|(_, (_, coverage, _))| {
            (coverage.grid.x, coverage.grid.y, coverage.quadrant)
        });
        let mut batches: Vec<PreparedBatch> = Vec::new();
        let mut tiles = HashMap::<IVec2, Vec<usize>>::new();
        for (primitive, (source_patch, coverage, selected)) in ordered {
            let (
                material,
                layers,
                tag,
                not_shadow_caster,
                not_shadow_receiver,
                transmitted_shadow_receiver,
                no_frustum_culling,
            ) = self
                .render_states
                .get(primitive)
                .map_err(|_| "terrain primitive lacks a standard material")?;
            if self
                .materials
                .get(&material.0)
                .is_none_or(|material| material.alpha_mode != AlphaMode::Opaque)
            {
                return Err("terrain material is missing or requires transparency ordering".into());
            }
            let source = meshes
                .get(
                    &mesh_handles
                        .get(primitive)
                        .map_err(|_| "terrain mesh disappeared")?
                        .0,
                )
                .ok_or("terrain mesh asset disappeared")?;
            if source
                .try_has_morph_targets()
                .map_err(|error| error.to_string())?
                || source.skinned_mesh_bounds().is_some()
                || source.contains_attribute(Mesh::ATTRIBUTE_JOINT_INDEX)
                || source.contains_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT)
            {
                return Err("terrain mesh has morph or skin data".into());
            }
            let state = RenderState {
                material: material.0.clone(),
                transform: primitive_transforms[&primitive],
                layers: layers.cloned(),
                tag: tag.map(|tag| tag.0),
                not_shadow_caster,
                not_shadow_receiver,
                transmitted_shadow_receiver,
                no_frustum_culling,
            };
            let tile = batch_tile(coverage.grid);
            let existing = tiles.get(&tile).and_then(|candidates| {
                candidates.iter().copied().find(|&index| {
                    batches[index].state == state
                        && compatible_vertex_streams(&batches[index].mesh, source)
                })
            });
            let index = match existing {
                Some(index) => index,
                None => {
                    let mut mesh = source.clone();
                    mesh.asset_usage =
                        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD;
                    mesh.insert_indices(Indices::U32(Vec::new()));
                    let index = batches.len();
                    batches.push(PreparedBatch {
                        tile,
                        state,
                        mesh,
                        members: Vec::new(),
                        selected: 0,
                    });
                    tiles.entry(tile).or_default().push(index);
                    index
                }
            };
            let batch = &mut batches[index];
            // The first source's vertices are already in the cloned mesh. Subsequent sources append
            // every attribute through Mesh::merge; compatibility was checked in both directions.
            let offset = if batch.members.is_empty() {
                0
            } else {
                batch.mesh.count_vertices() as u32
            };
            let indices = source
                .indices()
                .ok_or("terrain source is not indexed")?
                .iter()
                .map(|index| index as u32 + offset)
                .collect();
            let bounds = match source.attribute(Mesh::ATTRIBUTE_POSITION) {
                Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) => {
                    Aabb::enclosing(positions.iter().copied().map(Vec3::from_array))
                        .ok_or("terrain source has no positions")?
                }
                _ => return Err("terrain source has no Float32x3 positions".into()),
            };
            if !batch.members.is_empty() {
                batch
                    .mesh
                    .merge(source)
                    .map_err(|error| error.to_string())?;
            }
            if batch.members.len() == 16 {
                return Err("terrain tile covers more than sixteen quadrants".into());
            }
            if selected {
                batch.selected |= 1 << batch.members.len();
            }
            batch.members.push(BatchMember {
                source_patch,
                coverage,
                indices,
                bounds,
                source: mesh_handles
                    .get(primitive)
                    .map_err(|_| "terrain source disappeared")?
                    .0
                    .clone(),
            });
        }
        // Copy the selector's current source masks without rebuilding the tier map. Newly ready
        // or concurrently changed coverage still takes the full selector path later in this frame,
        // before initial activation is allowed to discard the original hierarchy.
        for batch in &mut batches {
            let (indices, bounds) = selected_geometry(&batch.members, batch.selected);
            batch.mesh.insert_indices(Indices::U32(indices));
            batch.mesh.final_aabb = bounds.map(Into::into);
        }
        Ok(batches)
    }
}

fn batch_tile(grid: IVec2) -> IVec2 {
    IVec2::new(
        grid.x.div_euclid(BATCH_SIDE_CELLS),
        grid.y.div_euclid(BATCH_SIDE_CELLS),
    )
}

fn compatible_vertex_streams(left: &Mesh, right: &Mesh) -> bool {
    left.primitive_topology() == right.primitive_topology()
        && left.enable_raytracing == right.enable_raytracing
        && left.attributes().count() == right.attributes().count()
        && left.attributes().zip(right.attributes()).all(
            |((left_attribute, left_values), (right_attribute, right_values))| {
                left_attribute.id == right_attribute.id
                    && left_attribute.format == right_attribute.format
                    && std::mem::discriminant(left_values) == std::mem::discriminant(right_values)
            },
        )
}

pub(super) fn install(
    root: Entity,
    source: Handle<WorldAsset>,
    prepared: Vec<PreparedBatch>,
    children: &Query<&Children>,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    readiness: Option<&TerrainMeshUploadReadiness>,
) {
    let source_children = children
        .get(root)
        .map(|children| children.iter().collect())
        .unwrap_or_default();
    let batches = prepared
        .into_iter()
        .map(|prepared| {
            let mesh = meshes.add(prepared.mesh);
            if let Some(readiness) = readiness {
                // Spread immutable source proof requests over the bounded CPU commits rather
                // than queuing all arrived roots during the first initial-activation poll.
                for member in &prepared.members {
                    readiness.request(member.source.id());
                }
                if prepared.selected != 0 {
                    readiness.request(mesh.id());
                }
            }
            let (_, bounds) = selected_geometry(&prepared.members, prepared.selected);
            let entity = spawn_render_mesh(
                commands,
                root,
                mesh.clone(),
                &prepared.state,
                Visibility::Hidden,
                bounds,
            );
            commands.entity(entity).insert((
                LodTerrainBatch,
                Name::new(format!(
                    "LOD terrain batch {},{}",
                    prepared.tile.x, prepared.tile.y
                )),
            ));
            TerrainBatch {
                entity,
                mesh,
                members: prepared.members,
                selected: prepared.selected,
                state: prepared.state,
                pending_upload: prepared.selected != 0,
                fallback_entities: Vec::new(),
            }
        })
        .collect();
    commands.entity(root).insert((
        LodTerrainBatches {
            batches,
            source_children: Some(source_children),
            _source: source,
        },
        TerrainSurfaceReady,
    ));
}

fn spawn_render_mesh(
    commands: &mut Commands,
    root: Entity,
    mesh: Handle<Mesh>,
    state: &RenderState,
    visibility: Visibility,
    bounds: Option<Aabb>,
) -> Entity {
    let mut entity = commands.spawn((
        NoAutoAabb,
        Mesh3d(mesh),
        MeshMaterial3d(state.material.clone()),
        state.transform,
        visibility,
        ChildOf(root),
    ));
    if let Some(bounds) = bounds {
        entity.insert(bounds);
    }
    if let Some(layers) = state.layers.clone() {
        entity.insert(layers);
    }
    if let Some(tag) = state.tag {
        entity.insert(MeshTag(tag));
    }
    if state.not_shadow_caster {
        entity.insert(NotShadowCaster);
    }
    if state.not_shadow_receiver {
        entity.insert(NotShadowReceiver);
    }
    if state.transmitted_shadow_receiver {
        entity.insert(TransmittedShadowReceiver);
    }
    if state.no_frustum_culling {
        entity.insert(NoFrustumCulling);
    }
    entity.id()
}

/// Root-level pending count used by the CPU preparation queue.
#[derive(SystemParam)]
pub(crate) struct LodBatchActivation<'w, 's> {
    chunks: Query<'w, 's, &'static LodTerrainBatches>,
}

impl LodBatchActivation<'_, '_> {
    pub(super) fn pending_count(&self) -> usize {
        self.chunks
            .iter()
            .filter(|chunk| chunk.source_children.is_some())
            .count()
    }
}

#[derive(SystemParam)]
pub(crate) struct LodBatchVisibility<'w, 's> {
    chunks: Query<
        'w,
        's,
        (
            Entity,
            &'static mut LodTerrainBatches,
            Option<&'static TerrainSurfaceReady>,
        ),
    >,
    removed: RemovedComponents<'w, 's, LodTerrainBatches>,
    source_states: Query<'w, 's, (&'static TerrainCoverage, Has<TerrainSurfaceReady>)>,
    meshes: Option<ResMut<'w, Assets<Mesh>>>,
    commands: Commands<'w, 's>,
    readiness: Option<Res<'w, TerrainMeshUploadReadiness>>,
}

impl LodBatchVisibility<'_, '_> {
    pub(super) fn pending_count(&self) -> usize {
        self.chunks
            .iter()
            .filter(|(_, chunk, _)| chunk.source_children.is_some())
            .count()
    }

    /// Immutable selection meshes still awaiting activation after their initial hierarchy transfer.
    pub(super) fn pending_selection_uploads(&self) -> usize {
        self.chunks
            .iter()
            .filter(|(_, chunk, _)| chunk.source_children.is_none())
            .flat_map(|(_, chunk, _)| &chunk.batches)
            .filter(|batch| batch.pending_upload)
            .count()
    }

    /// Run after semantic selection has updated every inactive mask. A transfer changes the
    /// representation of the same ready coverage, so its exact source removal events can be
    /// ignored by the next selector pass rather than rebuilding the tier map again.
    pub(super) fn finalize_one(&mut self, replaced_sources: &mut HashSet<Entity>) -> bool {
        let Some(readiness) = self.readiness.as_deref() else {
            return false;
        };
        if !readiness.has_render_backend() {
            return false;
        }
        let candidate = self.chunks.iter().find_map(|(entity, chunk, ready)| {
            chunk.source_children.as_ref()?;
            ready?;
            // CPU source readiness does not prove GPU preparation. Keep the source scene until
            // the immutable source buffers are prepared as well: later index-mask transitions
            // use those retained buffers for their exact coverage fallback.
            let mut sources_ready = true;
            for member in chunk.batches.iter().flat_map(|batch| &batch.members) {
                // An externally changed source keeps its hierarchy and readiness lifecycle.
                // Virtual coverage may replace only the exact validated ready source set.
                if !self
                    .source_states
                    .get(member.source_patch)
                    .is_ok_and(|(coverage, ready)| ready && *coverage == member.coverage)
                {
                    return None;
                }
                if !readiness.is_ready(member.source.id()) {
                    readiness.request(member.source.id());
                    sources_ready = false;
                }
            }
            (sources_ready
                && chunk
                    .batches
                    .iter()
                    .all(|batch| !batch.pending_upload || readiness.is_ready(batch.mesh.id())))
            .then_some(entity)
        });
        let Some(entity) = candidate else {
            return false;
        };
        let (_, mut chunk, _) = self.chunks.get_mut(entity).expect("selected chunk exists");
        // Unregister before despawning so the retained immutable GLB cannot respawn duplicates.
        self.commands.entity(entity).remove::<WorldAssetRoot>();
        self.commands.entity(entity).remove::<WorldInstance>();
        for child in chunk
            .source_children
            .take()
            .expect("initial source hierarchy exists")
        {
            self.commands.entity(child).try_despawn();
        }
        for batch in &mut chunk.batches {
            for member in &batch.members {
                replaced_sources.insert(member.source_patch);
                readiness.forget(member.source.id());
            }
            activate_uploaded_batch(batch, readiness, &mut self.commands);
        }
        true
    }
    pub(super) fn removed(&mut self) -> bool {
        self.removed.read().count() != 0
    }

    pub(super) fn upload_completed(&self) -> bool {
        let Some(readiness) = self.readiness.as_deref() else {
            return false;
        };
        if !readiness.has_ready_uploads() {
            return false;
        }
        self.chunks.iter().any(|(_, chunk, _)| {
            chunk.source_children.is_none()
                && chunk
                    .batches
                    .iter()
                    .any(|batch| batch.pending_upload && readiness.is_ready(batch.mesh.id()))
        })
    }

    pub(super) fn coverage(&self) -> impl Iterator<Item = TerrainCoverage> + '_ {
        self.chunks
            .iter()
            .filter(|(_, chunk, ready)| ready.is_some() && chunk.source_children.is_none())
            .flat_map(|(_, chunk, _)| {
                chunk
                    .batches
                    .iter()
                    .flat_map(|batch| batch.members.iter().map(|member| member.coverage))
            })
    }

    /// Inventory uses saved masks and member counts; it never visits vertex streams or tier maps.
    pub(super) fn counts(&self) -> (usize, usize, usize, usize) {
        self.chunks
            .iter()
            .fold((0, 0, 0, 0), |mut counts, (_, chunk, ready)| {
                let active = ready.is_some() && chunk.source_children.is_none();
                for batch in &chunk.batches {
                    counts.0 += batch.members.len();
                    counts.1 += 1;
                    if active {
                        counts.2 += batch.selected.count_ones() as usize;
                        counts.3 += usize::from(batch.selected != 0 && !batch.pending_upload);
                    }
                }
                counts
            })
    }

    /// Upload completion cannot change selected coverage. Activate only the stored current IDs;
    /// a semantic change always updates masks first through `update` instead of this fast path.
    pub(super) fn finish_uploads(&mut self) {
        let Some(readiness) = self.readiness.as_deref() else {
            return;
        };
        if !readiness.has_ready_uploads() {
            return;
        }
        for (_, mut chunk, ready) in &mut self.chunks {
            if ready.is_none() || chunk.source_children.is_some() {
                continue;
            }
            for batch in &mut chunk.batches {
                if batch.pending_upload && readiness.is_ready(batch.mesh.id()) {
                    activate_uploaded_batch(batch, readiness, &mut self.commands);
                }
            }
        }
    }

    pub(super) fn update(
        &mut self,
        selected_tiers: &HashMap<(IVec2, u8), Option<LodTier>>,
    ) -> (usize, usize, usize) {
        let Some(meshes) = self.meshes.as_deref_mut() else {
            return (0, 0, 0);
        };
        let Some(readiness) = self.readiness.as_deref() else {
            return (0, 0, 0);
        };
        let mut visible_quadrants = 0;
        let mut visible_batches = 0;
        let mut changed_batches = 0;
        for (root, mut chunk, ready) in &mut self.chunks {
            let initial_upload = chunk.source_children.is_some();
            for batch in &mut chunk.batches {
                let selected =
                    batch
                        .members
                        .iter()
                        .enumerate()
                        .fold(0u16, |mask, (index, member)| {
                            if ready.is_some()
                                && selected_tiers
                                    .get(&(member.coverage.grid, member.coverage.quadrant))
                                    == Some(&member.coverage.tier)
                            {
                                mask | (1 << index)
                            } else {
                                mask
                            }
                        });
                if !initial_upload {
                    visible_quadrants += selected.count_ones() as usize;
                }
                if selected != batch.selected {
                    let Some(source_geometry) = meshes.get(&batch.mesh) else {
                        continue;
                    };
                    let mut replacement = source_geometry.clone();
                    let (indices, bounds) = selected_geometry(&batch.members, selected);
                    replacement.insert_indices(Indices::U32(indices));
                    replacement.final_aabb = bounds.map(Into::into);
                    let replacement = meshes.add(replacement);
                    // Every pending mask has a new immutable ID. A GPU-ready acknowledgment can
                    // therefore never refer to old or deferred contents on a reused asset handle.
                    readiness.forget(batch.mesh.id());
                    meshes.remove(batch.mesh.id());
                    batch.mesh = replacement;
                    batch.selected = selected;
                    batch.pending_upload = selected != 0;
                    self.commands
                        .entity(batch.entity)
                        .insert((Mesh3d(batch.mesh.clone()), Visibility::Hidden));
                    for fallback in batch.fallback_entities.drain(..) {
                        self.commands.entity(fallback).try_despawn();
                    }
                    if selected != 0 {
                        readiness.request(batch.mesh.id());
                        if !initial_upload {
                            // Source assets were already drawable and are retained for the chunk's
                            // lifetime. Draw the exact newly selected coverage while its merged GPU
                            // buffers wait for upload; old batch triangles stay hidden.
                            for (index, member) in batch.members.iter().enumerate() {
                                if selected & (1 << index) != 0 {
                                    batch.fallback_entities.push(spawn_render_mesh(
                                        &mut self.commands,
                                        root,
                                        member.source.clone(),
                                        &batch.state,
                                        Visibility::Inherited,
                                        Some(member.bounds),
                                    ));
                                }
                            }
                        }
                    }
                    changed_batches += 1;
                } else if !initial_upload
                    && batch.pending_upload
                    && readiness.is_ready(batch.mesh.id())
                {
                    activate_uploaded_batch(batch, readiness, &mut self.commands);
                }
                if !initial_upload {
                    visible_batches += usize::from(selected != 0 && !batch.pending_upload);
                }
            }
        }
        (visible_quadrants, visible_batches, changed_batches)
    }
}

fn activate_uploaded_batch(
    batch: &mut TerrainBatch,
    readiness: &TerrainMeshUploadReadiness,
    commands: &mut Commands,
) {
    if batch.pending_upload && !readiness.is_ready(batch.mesh.id()) {
        return;
    }
    readiness.forget(batch.mesh.id());
    batch.pending_upload = false;
    for fallback in batch.fallback_entities.drain(..) {
        commands.entity(fallback).try_despawn();
    }
    let (_, bounds) = selected_geometry(&batch.members, batch.selected);
    if let Some(bounds) = bounds {
        commands.entity(batch.entity).insert((
            Mesh3d(batch.mesh.clone()),
            bounds,
            Visibility::Inherited,
        ));
    } else {
        commands.entity(batch.entity).insert(Visibility::Hidden);
    }
}

fn selected_geometry(members: &[BatchMember], selected: u16) -> (Vec<u32>, Option<Aabb>) {
    let mut indices = Vec::new();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for (index, member) in members.iter().enumerate() {
        if selected & (1 << index) != 0 {
            indices.extend_from_slice(&member.indices);
            min = min.min(member.bounds.min().into());
            max = max.max(member.bounds.max().into());
        }
    }
    let bounds = (!indices.is_empty()).then(|| Aabb::from_min_max(min, max));
    (indices, bounds)
}

#[cfg(test)]
mod tests {
    use super::super::{LodStreaming, update_terrain_lod_visibility};
    use super::*;
    use crate::{
        config::EngineConfig,
        profiling::ProfilingState,
        streaming::{RenderOrigin, StreamingCommitBudget, StreamingMetrics},
        world::components::{CELL_SIZE, StreamingCamera, TerrainPatch},
    };
    use bevy::{ecs::system::RunSystemOnce, mesh::PrimitiveTopology};
    use shared::lod::{TERRAIN_QUADRANT_INDEX_COUNT, TERRAIN_QUADRANT_VERTEX_COUNT};

    struct Fixture {
        root: Entity,
        scene_root: Entity,
        patches: Vec<(Entity, TerrainCoverage)>,
        primitives: Vec<Entity>,
        mesh_handles: Vec<Handle<Mesh>>,
        source: Handle<WorldAsset>,
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<WorldAsset>>()
            .init_resource::<LodStreaming>()
            .init_resource::<TerrainMeshUploadReadiness>();
        app.world()
            .resource::<TerrainMeshUploadReadiness>()
            .enable_test_render_backend();
        app
    }

    fn source_mesh(grid: IVec2, quadrant: u8) -> Mesh {
        let mut positions: Vec<_> = (0..64)
            .map(|index| {
                let (x, y) = match index {
                    0..=16 => (index, 0),
                    17..=32 => (16, index - 16),
                    33..=48 => (48 - index, 16),
                    _ => (0, 64 - index),
                };
                [
                    grid.x as f32 * CELL_SIZE
                        + f32::from(quadrant % 2) * CELL_SIZE * 0.5
                        + x as f32 * CELL_SIZE / 32.0,
                    grid.y as f32 * CELL_SIZE
                        + f32::from(quadrant / 2) * CELL_SIZE * 0.5
                        + y as f32 * CELL_SIZE / 32.0,
                    10.0 + (index as f32 * 0.5),
                ]
            })
            .collect();
        positions.push([
            grid.x as f32 * CELL_SIZE
                + f32::from(quadrant % 2) * CELL_SIZE * 0.5
                + CELL_SIZE * 0.25,
            grid.y as f32 * CELL_SIZE
                + f32::from(quadrant / 2) * CELL_SIZE * 0.5
                + CELL_SIZE * 0.25,
            12.0,
        ]);
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 65])
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_UV_0,
            (0..65)
                .map(|index| [index as f32 / 64.0, 0.5])
                .collect::<Vec<_>>(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.2, 0.7]; 65])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.5, 0.75, 0.8, 1.0]; 65])
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0, 0.0, 0.0, -1.0]; 65])
        .with_inserted_indices(Indices::U32(
            (0..64)
                .flat_map(|edge| [64, edge, (edge + 1) % 64])
                .collect(),
        ))
    }

    fn fixture(app: &mut App, grids: &[IVec2], tier: LodTier) -> Fixture {
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let root = app
            .world_mut()
            .spawn((
                Transform::from_xyz(1000.0, 0.0, -2000.0),
                Visibility::Hidden,
            ))
            .id();
        let loader_scene = app
            .world_mut()
            .spawn((
                Name::new("chunk_4_0_0"),
                Transform::IDENTITY,
                Visibility::Inherited,
                ChildOf(root),
            ))
            .id();
        let scene_root = app
            .world_mut()
            .spawn((
                Name::new("chunk_4_0_0"),
                Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
                Visibility::Inherited,
                ChildOf(loader_scene),
            ))
            .id();
        let mut patches = Vec::new();
        let mut primitives = Vec::new();
        let mut mesh_handles = Vec::new();
        let mut source_world = World::new();
        for &grid in grids {
            let cell = app
                .world_mut()
                .spawn((
                    Name::new(nodes::source_cell(grid.x, grid.y)),
                    Transform::IDENTITY,
                    Visibility::Inherited,
                    ChildOf(scene_root),
                ))
                .id();
            let group = app
                .world_mut()
                .spawn((
                    Name::new(nodes::terrain_group()),
                    Transform::IDENTITY,
                    Visibility::Inherited,
                    ChildOf(cell),
                ))
                .id();
            for (quadrant, name) in ["sw", "se", "nw", "ne"].into_iter().enumerate() {
                let quadrant = quadrant as u8;
                let patch = app
                    .world_mut()
                    .spawn((
                        Name::new(format!("terrain_quadrant_{name}")),
                        Transform::IDENTITY,
                        Visibility::Inherited,
                        ChildOf(group),
                    ))
                    .id();
                let mesh = app
                    .world_mut()
                    .resource_mut::<Assets<Mesh>>()
                    .add(source_mesh(grid, quadrant));
                let primitive = app
                    .world_mut()
                    .spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material.clone()),
                        Transform::IDENTITY,
                        Visibility::Inherited,
                        ChildOf(patch),
                    ))
                    .id();
                source_world.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
                primitives.push(primitive);
                mesh_handles.push(mesh);
                patches.push((
                    patch,
                    TerrainCoverage {
                        grid,
                        quadrant,
                        tier: Some(tier),
                    },
                ));
            }
        }
        let source = app
            .world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .add(WorldAsset::new(source_world));
        app.world_mut()
            .entity_mut(root)
            .insert(WorldAssetRoot(source.clone()));
        Fixture {
            root,
            scene_root,
            patches,
            primitives,
            mesh_handles,
            source,
        }
    }

    fn prepare(app: &mut App, fixture: &Fixture) -> Result<Vec<PreparedBatch>, String> {
        let root = fixture.root;
        let patches = fixture.patches.clone();
        app.world_mut()
            .run_system_once(
                move |preparation: LodBatchPreparation,
                      children: Query<&Children>,
                      names: Query<&Name>,
                      mesh_handles: Query<&Mesh3d>,
                      meshes: Res<Assets<Mesh>>| {
                    preparation.prepare(root, &patches, &children, &names, &mesh_handles, &meshes)
                },
            )
            .unwrap()
    }

    fn install_fixture(app: &mut App, fixture: &Fixture, mut batches: Vec<PreparedBatch>) {
        for &(entity, coverage) in &fixture.patches {
            app.world_mut()
                .entity_mut(entity)
                .insert((coverage, TerrainSurfaceReady));
        }
        app.world_mut()
            .entity_mut(fixture.root)
            .insert(Visibility::Inherited);
        let root = fixture.root;
        let source = fixture.source.clone();
        app.world_mut()
            .run_system_once(
                move |children: Query<&Children>,
                      mut commands: Commands,
                      mut meshes: ResMut<Assets<Mesh>>,
                      readiness: Res<TerrainMeshUploadReadiness>| {
                    install(
                        root,
                        source.clone(),
                        std::mem::take(&mut batches),
                        &children,
                        &mut commands,
                        &mut meshes,
                        Some(&readiness),
                    );
                },
            )
            .unwrap();
    }

    fn acknowledge_uploads(app: &App, root: Entity) {
        let batches = app.world().get::<LodTerrainBatches>(root).unwrap();
        let readiness = app.world().resource::<TerrainMeshUploadReadiness>();
        for batch in &batches.batches {
            if batch.pending_upload && !readiness.is_ready(batch.mesh.id()) {
                assert!(readiness.acknowledge(batch.mesh.id()));
            }
            if batches.source_children.is_some() {
                for member in &batch.members {
                    readiness.request(member.source.id());
                    if !readiness.is_ready(member.source.id()) {
                        assert!(readiness.acknowledge(member.source.id()));
                    }
                }
            }
        }
    }

    fn finalize_initial(app: &mut App) -> bool {
        app.world_mut()
            .run_system_once(
                |mut batches: LodBatchVisibility, mut streaming: ResMut<LodStreaming>| {
                    batches.finalize_one(&mut streaming.replaced_source_quadrants)
                },
            )
            .unwrap()
    }

    fn reset_submission_budget(mut budget: ResMut<StreamingCommitBudget>) {
        budget.remaining = 4;
        budget.commits = 0;
        budget.frame_started = std::time::Instant::now();
    }

    fn selection_app() -> App {
        use super::super::batch_ready_lod_chunks;
        let mut app = test_app();
        app.insert_resource(EngineConfig {
            max_commit_micros_per_frame: 1_000_000,
            ..default()
        })
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<ProfilingState>()
        .add_systems(
            Update,
            (
                reset_submission_budget,
                batch_ready_lod_chunks,
                update_terrain_lod_visibility,
            )
                .chain(),
        );
        app.world_mut().spawn((
            StreamingCamera,
            Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
        ));
        app
    }

    fn ready_source(app: &mut App, fixture: &Fixture) {
        for &(entity, coverage) in &fixture.patches {
            app.world_mut()
                .entity_mut(entity)
                .insert((coverage, TerrainSurfaceReady));
        }
        app.world_mut()
            .entity_mut(fixture.root)
            .insert(Visibility::Inherited);
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
    }

    fn queue_source(app: &mut App, fixture: &Fixture) {
        use super::super::LodChunkRoot;
        use shared::lod::{ChunkAnchor, ChunkKey, LodOrigin};
        let tier = fixture.patches[0].1.tier.unwrap();
        app.world_mut().entity_mut(fixture.root).insert((
            LodChunkRoot {
                key: ChunkKey::new(1, tier, ChunkAnchor::new(0, 0)),
                generation: 1,
                origin: LodOrigin::new(0, 0),
                retry_count: 0,
            },
            PendingLodTerrainBatching {
                source: fixture.source.clone(),
                patches: fixture.patches.clone(),
            },
        ));
    }

    fn full_quadrant(app: &mut App, grid: IVec2, quadrant: u8) -> Entity {
        app.world_mut()
            .spawn((
                TerrainPatch,
                TerrainCoverage {
                    grid,
                    quadrant,
                    tier: None,
                },
                TerrainSurfaceReady,
            ))
            .id()
    }

    #[test]
    fn selection_upload_gauge_separates_initial_chunks_and_tracks_activation() {
        let mut app = selection_app();
        let fixture = fixture(&mut app, &[IVec2::ZERO, IVec2::new(3, 0)], LodTier::Tier4);
        let output = tempfile::tempdir().unwrap();
        let assert_gauges = |app: &App, initial: f64, selection: f64| {
            let config = EngineConfig {
                profile_output_dir: Some(output.path().to_owned()),
                ..default()
            };
            app.world()
                .resource::<ProfilingState>()
                .write_bundle(
                    &config,
                    &serde_json::json!({"average_fps": 90.0, "frame_ms_p95": 14.0, "passed": true}),
                    Some(app.world().resource::<StreamingMetrics>()),
                    &crate::render::RendererMetrics::default(),
                    None,
                )
                .unwrap();
            let report: serde_json::Value = serde_json::from_slice(
                &std::fs::read(output.path().join("cpu-spans.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                report["gauges"]["lod/pending_initial_terrain_upload_chunks"],
                initial
            );
            assert_eq!(
                report["gauges"]["lod/pending_terrain_selection_uploads"],
                selection
            );
        };

        ready_source(&mut app, &fixture);
        queue_source(&mut app, &fixture);
        app.update();
        // Preparation consumes this frame's commit slot; sample the retained initial queue
        // on the next update while all mesh acknowledgments are still withheld.
        app.update();
        assert_gauges(&app, 1.0, 0.0);
        acknowledge_uploads(&app, fixture.root);
        app.update();
        assert_gauges(&app, 0.0, 0.0);

        full_quadrant(&mut app, IVec2::ZERO, 0);
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        assert_gauges(&app, 0.0, 1.0);
        full_quadrant(&mut app, IVec2::new(3, 0), 0);
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        assert_gauges(&app, 0.0, 2.0);
        let revision = app.world().resource::<LodStreaming>().visibility_revision;
        acknowledge_uploads(&app, fixture.root);
        app.update();
        assert_gauges(&app, 0.0, 0.0);
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision
        );
    }

    #[test]
    fn stable_conversion_and_transfer_preserve_mixed_empty_masks_without_tier_rebuilds() {
        let mut app = selection_app();
        let fixture = fixture(&mut app, &[IVec2::ZERO, IVec2::new(3, 0)], LodTier::Tier4);
        ready_source(&mut app, &fixture);
        full_quadrant(&mut app, IVec2::ZERO, 0);
        for quadrant in 0..4 {
            full_quadrant(&mut app, IVec2::new(3, 0), quadrant);
        }
        app.update();
        let revision = app.world().resource::<LodStreaming>().visibility_revision;
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            3
        );

        queue_source(&mut app, &fixture);
        app.update();
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision
        );
        assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
        let (generated, mixed_entity, empty_entity) = {
            let chunk = app.world().get::<LodTerrainBatches>(fixture.root).unwrap();
            assert!(chunk.source_children.is_some());
            assert_eq!(chunk.batches.len(), 2);
            assert_eq!(chunk.batches[0].selected, 0b1110);
            assert_eq!(chunk.batches[1].selected, 0);
            assert!(chunk.batches[0].pending_upload);
            assert!(!chunk.batches[1].pending_upload);
            for batch in &chunk.batches {
                assert_eq!(
                    app.world()
                        .resource::<Assets<Mesh>>()
                        .get(&batch.mesh)
                        .unwrap()
                        .indices()
                        .unwrap()
                        .len(),
                    batch.selected.count_ones() as usize * TERRAIN_QUADRANT_INDEX_COUNT
                );
            }
            (
                chunk
                    .batches
                    .iter()
                    .map(|batch| batch.mesh.id())
                    .collect::<Vec<_>>(),
                chunk.batches[0].entity,
                chunk.batches[1].entity,
            )
        };
        assert!(
            fixture
                .primitives
                .iter()
                .all(|&entity| app.world().get_entity(entity).is_ok())
        );
        acknowledge_uploads(&app, fixture.root);
        app.update();
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision
        );
        assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
        assert!(
            fixture
                .primitives
                .iter()
                .all(|&entity| app.world().get_entity(entity).is_err())
        );
        assert_eq!(
            *app.world().get::<Visibility>(mixed_entity).unwrap(),
            Visibility::Inherited
        );
        assert_eq!(
            *app.world().get::<Visibility>(empty_entity).unwrap(),
            Visibility::Hidden
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            3
        );
        assert_eq!(
            app.world()
                .resource::<LodStreaming>()
                .replaced_source_quadrants
                .len(),
            fixture.patches.len()
        );

        app.update();
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision,
            "the two expected removal streams do not rebuild the same tier map"
        );
        assert!(
            app.world()
                .resource::<LodStreaming>()
                .replaced_source_quadrants
                .is_empty()
        );
        app.world_mut().entity_mut(fixture.root).despawn();
        assert!(
            generated
                .iter()
                .all(|&id| app.world().resource::<Assets<Mesh>>().get(id).is_none())
        );
        app.update();
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision + 1,
            "real root removal still invalidates available tiers"
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            0
        );
    }

    #[test]
    fn initial_acknowledgments_wait_for_current_camera_and_readiness_masks() {
        // Both cells are inside one compiler tier-4 chunk, but initially only the first is in
        // the default twelve-cell reach. A ready old mask must not flatten the hierarchy before
        // the newly in-range second cell's nonempty replacement has uploaded.
        {
            let mut app = selection_app();
            let fixture = fixture(&mut app, &[IVec2::ZERO, IVec2::new(3, 0)], LodTier::Tier4);
            {
                let world = app.world_mut();
                let mut cameras = world.query_filtered::<&mut Transform, With<StreamingCamera>>();
                cameras.single_mut(world).unwrap().translation.x = -9.5 * CELL_SIZE;
            }
            ready_source(&mut app, &fixture);
            queue_source(&mut app, &fixture);
            app.update();
            {
                let chunk = app.world().get::<LodTerrainBatches>(fixture.root).unwrap();
                assert_eq!(chunk.batches[0].selected, 0b1111);
                assert_eq!(
                    chunk.batches[1].selected, 0,
                    "same-frame readiness corrects preparation's inherited source visibility"
                );
            }
            acknowledge_uploads(&app, fixture.root);
            let old_far = app
                .world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .batches[1]
                .mesh
                .id();
            {
                let world = app.world_mut();
                let mut cameras = world.query_filtered::<&mut Transform, With<StreamingCamera>>();
                cameras.single_mut(world).unwrap().translation.x = 0.5 * CELL_SIZE;
            }
            app.update();
            let chunk = app.world().get::<LodTerrainBatches>(fixture.root).unwrap();
            assert!(chunk.source_children.is_some());
            assert_eq!(chunk.batches[1].selected, 0b1111);
            assert!(chunk.batches[1].pending_upload);
            assert_ne!(chunk.batches[1].mesh.id(), old_far);
            assert!(
                fixture
                    .primitives
                    .iter()
                    .all(|&entity| app.world().get_entity(entity).is_ok())
            );
            assert_eq!(
                app.world()
                    .resource::<StreamingMetrics>()
                    .visible_lod_terrain_patches,
                8
            );
            acknowledge_uploads(&app, fixture.root);
            app.update();
            assert!(
                app.world()
                    .get::<LodTerrainBatches>(fixture.root)
                    .unwrap()
                    .source_children
                    .is_none()
            );
        }

        // A full-detail surface loses readiness on the same update that yesterday's initial
        // three-quadrant upload is acknowledged. The new four-quadrant mask remains inactive.
        let mut app = selection_app();
        let fixture = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier4);
        let near = full_quadrant(&mut app, IVec2::ZERO, 0);
        ready_source(&mut app, &fixture);
        queue_source(&mut app, &fixture);
        app.update();
        let old = app
            .world()
            .get::<LodTerrainBatches>(fixture.root)
            .unwrap()
            .batches[0]
            .mesh
            .id();
        acknowledge_uploads(&app, fixture.root);
        app.world_mut()
            .entity_mut(near)
            .remove::<TerrainSurfaceReady>();
        app.update();
        let chunk = app.world().get::<LodTerrainBatches>(fixture.root).unwrap();
        assert!(chunk.source_children.is_some());
        assert_eq!(chunk.batches[0].selected, 0b1111);
        assert_ne!(chunk.batches[0].mesh.id(), old);
        assert!(
            !app.world()
                .resource::<TerrainMeshUploadReadiness>()
                .is_ready(old)
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            4
        );
        assert!(
            fixture
                .primitives
                .iter()
                .all(|&entity| app.world().get_entity(entity).is_ok())
        );
        acknowledge_uploads(&app, fixture.root);
        app.update();
        assert!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .source_children
                .is_none()
        );
    }

    #[test]
    fn source_readiness_loss_is_not_suppressed_and_upload_only_completion_avoids_rebuilds() {
        let mut app = selection_app();
        let fixture = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier4);
        ready_source(&mut app, &fixture);
        queue_source(&mut app, &fixture);
        app.update();
        acknowledge_uploads(&app, fixture.root);
        let source = fixture.patches[0].0;
        app.world_mut()
            .entity_mut(source)
            .remove::<TerrainSurfaceReady>();
        let revision = app.world().resource::<LodStreaming>().visibility_revision;
        app.update();
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision + 1
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            3
        );
        assert!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .source_children
                .is_some()
        );
        acknowledge_uploads(&app, fixture.root);
        app.update();
        assert!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .source_children
                .is_some(),
            "an externally unready source keeps its original readiness lifecycle"
        );
        assert!(
            app.world()
                .resource::<LodStreaming>()
                .replaced_source_quadrants
                .is_empty()
        );
        app.world_mut()
            .entity_mut(source)
            .insert(TerrainSurfaceReady);
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        acknowledge_uploads(&app, fixture.root);
        app.update();
        assert!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .source_children
                .is_none()
        );
        app.update();
        let near = full_quadrant(&mut app, IVec2::ZERO, 0);
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        let revision = app.world().resource::<LodStreaming>().visibility_revision;
        assert_eq!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .batches[0]
                .fallback_entities
                .len(),
            3
        );
        acknowledge_uploads(&app, fixture.root);
        app.update();
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision,
            "ready current uploads need no new tier map"
        );
        assert!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .batches[0]
                .fallback_entities
                .is_empty()
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            3
        );
        // A later real coverage removal is independent of the retired source-ID transfer set.
        app.world_mut().entity_mut(near).remove::<TerrainCoverage>();
        app.update();
        assert_eq!(
            app.world().resource::<LodStreaming>().visibility_revision,
            revision + 1
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            4
        );
    }

    #[test]
    fn transfer_tracking_expires_with_missing_events_and_without_a_camera() {
        let mut app = selection_app();
        app.update();
        let absent = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .replaced_source_quadrants
            .insert(absent);
        app.update();
        assert!(
            app.world()
                .resource::<LodStreaming>()
                .replaced_source_quadrants
                .is_empty(),
            "neither stream produced an event, so no tracking survives"
        );
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .replaced_source_quadrants
            .insert(absent);
        let mut cameras = app
            .world_mut()
            .query_filtered::<Entity, With<StreamingCamera>>();
        let camera = cameras.single(app.world()).unwrap();
        app.world_mut().entity_mut(camera).despawn();
        let real = full_quadrant(&mut app, IVec2::ZERO, 0);
        app.world_mut()
            .entity_mut(real)
            .remove::<TerrainSurfaceReady>();
        app.update();
        assert!(
            app.world()
                .resource::<LodStreaming>()
                .replaced_source_quadrants
                .is_empty()
        );
        assert!(
            app.world().resource::<LodStreaming>().visibility_dirty,
            "real removals remain pending semantically when no camera can run selection"
        );
    }

    #[test]
    fn batches_preserve_every_vertex_attribute_triangle_and_quadrant() {
        let mut app = test_app();
        let fixture = fixture(
            &mut app,
            &[IVec2::ZERO, IVec2::X, IVec2::Y, IVec2::ONE],
            LodTier::Tier4,
        );
        let prepared = prepare(&mut app, &fixture).unwrap();
        assert_eq!(prepared.len(), 1);
        let batch = &prepared[0];
        assert_eq!(batch.members.len(), 16);
        assert_eq!(
            batch.mesh.count_vertices(),
            16 * TERRAIN_QUADRANT_VERTEX_COUNT
        );
        let meshes = app.world().resource::<Assets<Mesh>>();
        let sources: HashMap<_, _> = fixture
            .patches
            .iter()
            .zip(&fixture.mesh_handles)
            .map(|((_, coverage), handle)| {
                (
                    (coverage.grid, coverage.quadrant),
                    meshes.get(handle).unwrap(),
                )
            })
            .collect();
        for (attribute, values) in batch.mesh.attributes() {
            let mut expected_bytes = Vec::new();
            for member in &batch.members {
                expected_bytes.extend_from_slice(
                    sources[&(member.coverage.grid, member.coverage.quadrant)]
                        .attribute(attribute.id)
                        .unwrap()
                        .get_bytes(),
                );
            }
            assert_eq!(
                values.get_bytes(),
                expected_bytes,
                "attribute {} is preserved byte for byte",
                attribute.name
            );
        }
        for (index, member) in batch.members.iter().enumerate() {
            let source = sources[&(member.coverage.grid, member.coverage.quadrant)];
            let expected: Vec<_> = source
                .indices()
                .unwrap()
                .iter()
                .map(|source_index| (source_index + index * TERRAIN_QUADRANT_VERTEX_COUNT) as u32)
                .collect();
            assert_eq!(member.indices, expected);
        }
        let (indices, _) = selected_geometry(&batch.members, u16::MAX);
        assert_eq!(indices.len(), 16 * TERRAIN_QUADRANT_INDEX_COUNT);
        let (partial, bounds) = selected_geometry(&batch.members, 0b1001);
        assert_eq!(
            partial,
            [
                batch.members[0].indices.clone(),
                batch.members[3].indices.clone()
            ]
            .concat()
        );
        let bounds = bounds.unwrap();
        assert_eq!(
            bounds.min(),
            batch.members[0]
                .bounds
                .min()
                .min(batch.members[3].bounds.min())
        );
        assert_eq!(
            bounds.max(),
            batch.members[0]
                .bounds
                .max()
                .max(batch.members[3].bounds.max())
        );
        assert_eq!(selected_geometry(&batch.members, 0), (Vec::new(), None));
    }

    #[test]
    fn negative_tiles_and_incompatible_materials_or_flags_remain_separate() {
        assert_eq!(batch_tile(IVec2::new(-1, -1)), IVec2::new(-1, -1));
        assert_eq!(batch_tile(IVec2::new(-2, -2)), IVec2::new(-1, -1));
        assert_eq!(batch_tile(IVec2::new(-3, 0)), IVec2::new(-2, 0));
        let mut app = test_app();
        let fixture = fixture(
            &mut app,
            &[IVec2::new(-2, -2), IVec2::new(-1, -1), IVec2::ZERO],
            LodTier::Tier4,
        );
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        app.world_mut()
            .entity_mut(fixture.primitives[0])
            .insert((MeshMaterial3d(material), NotShadowCaster));
        app.world_mut().entity_mut(fixture.primitives[1]).insert((
            NotShadowReceiver,
            RenderLayers::layer(3),
            MeshTag(7),
        ));
        let batches = prepare(&mut app, &fixture).unwrap();
        assert_eq!(
            batches.len(),
            4,
            "two tiles plus two distinct primitive states"
        );
        let shadow_excluded = batches
            .iter()
            .find(|batch| batch.state.not_shadow_caster)
            .unwrap();
        assert_eq!(shadow_excluded.members.len(), 1);
        let tagged = batches
            .iter()
            .find(|batch| batch.state.tag == Some(7))
            .unwrap();
        assert!(tagged.state.not_shadow_receiver);
        assert_eq!(tagged.state.layers, Some(RenderLayers::layer(3)));
        assert!(batches.iter().all(|batch| {
            batch
                .members
                .iter()
                .all(|member| batch_tile(member.coverage.grid) == batch.tile)
        }));
    }

    #[test]
    fn unsupported_scene_behavior_leaves_the_original_hierarchy_untouched() {
        let mut app = test_app();
        let fixture = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier4);
        let original_entities = app.world().entities().len();
        let mut checks = 0;
        app.world_mut()
            .entity_mut(fixture.primitives[0])
            .insert(NoAutomaticBatching);
        assert!(
            prepare(&mut app, &fixture)
                .err()
                .unwrap()
                .contains("render behavior")
        );
        app.world_mut()
            .entity_mut(fixture.primitives[0])
            .remove::<NoAutomaticBatching>();
        checks += 1;
        app.world_mut()
            .entity_mut(fixture.primitives[0])
            .insert(Transform::from_scale(Vec3::new(1.0, 2.0, 1.0)));
        assert!(
            prepare(&mut app, &fixture)
                .err()
                .unwrap()
                .contains("local transforms")
        );
        app.world_mut()
            .entity_mut(fixture.primitives[0])
            .insert(Transform::IDENTITY);
        checks += 1;
        let material = app
            .world()
            .get::<MeshMaterial3d<StandardMaterial>>(fixture.primitives[0])
            .unwrap()
            .0
            .clone();
        app.world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .get_mut(&material)
            .unwrap()
            .alpha_mode = AlphaMode::Blend;
        assert!(
            prepare(&mut app, &fixture)
                .err()
                .unwrap()
                .contains("transparency ordering")
        );
        app.world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .get_mut(&material)
            .unwrap()
            .alpha_mode = AlphaMode::Opaque;
        checks += 1;
        let objects = app
            .world_mut()
            .spawn((
                Name::new("objects"),
                Transform::IDENTITY,
                ChildOf(fixture.scene_root),
            ))
            .id();
        assert!(
            prepare(&mut app, &fixture)
                .err()
                .unwrap()
                .contains("non-terrain node")
        );
        app.world_mut().entity_mut(objects).despawn();
        checks += 1;
        assert_eq!(checks, 4);
        assert_eq!(app.world().entities().len(), original_entities);
        assert!(app.world().get::<WorldAssetRoot>(fixture.root).is_some());
        assert!(
            fixture
                .primitives
                .iter()
                .all(|&entity| app.world().get_entity(entity).is_ok())
        );
        assert!(prepare(&mut app, &fixture).is_ok());
        app.world_mut()
            .entity_mut(fixture.primitives[0])
            .insert(NoCpuCulling);
        assert!(
            prepare(&mut app, &fixture).is_err(),
            "explicit mesh culling behavior is retained by fallback"
        );
        app.world_mut()
            .entity_mut(fixture.primitives[0])
            .remove::<NoCpuCulling>();
        app.world_mut()
            .entity_mut(fixture.scene_root)
            .insert((NoCpuCulling, StructuralHierarchyNode));
        assert!(
            prepare(&mut app, &fixture).is_ok(),
            "owned empty hierarchy optimization is compatible"
        );
        app.world_mut()
            .entity_mut(fixture.scene_root)
            .insert(Collider::ball(1.0));
        assert!(
            prepare(&mut app, &fixture).is_err(),
            "simulation content cannot be flattened"
        );
    }

    #[test]
    fn deferred_uploads_keep_exact_coverage_and_superseded_masks_cannot_activate() {
        let mut app = test_app();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .init_resource::<LodStreaming>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, update_terrain_lod_visibility);
        app.world_mut().spawn((
            StreamingCamera,
            Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
        ));
        let fixture = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier4);
        let prepared = prepare(&mut app, &fixture).unwrap();
        install_fixture(&mut app, &fixture, prepared);
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        let entity = app
            .world()
            .get::<LodTerrainBatches>(fixture.root)
            .unwrap()
            .batches[0]
            .entity;
        assert!(
            !finalize_initial(&mut app),
            "no GPU acknowledgment cannot discard original terrain"
        );
        assert!(
            fixture
                .primitives
                .iter()
                .all(|&entity| app.world().get_entity(entity).is_ok())
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            4
        );
        assert_eq!(
            *app.world().get::<Visibility>(entity).unwrap(),
            Visibility::Hidden
        );
        {
            let batches = app.world().get::<LodTerrainBatches>(fixture.root).unwrap();
            let readiness = app.world().resource::<TerrainMeshUploadReadiness>();
            assert!(readiness.acknowledge(batches.batches[0].mesh.id()));
        }
        assert!(
            !finalize_initial(&mut app),
            "prepared merged buffers cannot discard unprepared fallback source buffers"
        );
        acknowledge_uploads(&app, fixture.root);
        assert!(finalize_initial(&mut app));
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        assert!(
            fixture
                .primitives
                .iter()
                .all(|&entity| app.world().get_entity(entity).is_err())
        );
        assert_eq!(app.world().get::<Children>(fixture.root).unwrap().len(), 1);
        assert!(app.world().get::<WorldAssetRoot>(fixture.root).is_none());
        let original = app.world().get::<Mesh3d>(entity).unwrap().0.clone();
        let full_detail = app
            .world_mut()
            .spawn((
                TerrainPatch,
                TerrainCoverage {
                    grid: IVec2::ZERO,
                    quadrant: 0,
                    tier: None,
                },
                TerrainSurfaceReady,
            ))
            .id();
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        let pending = app.world().get::<Mesh3d>(entity).unwrap().0.clone();
        assert_ne!(
            pending.id(),
            original.id(),
            "each uploaded mask has an immutable generation ID"
        );
        assert!(
            app.world()
                .resource::<Assets<Mesh>>()
                .get(&original)
                .is_none()
        );
        {
            let batch = &app
                .world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .batches[0];
            assert_eq!(batch.fallback_entities.len(), 3);
            assert_eq!(
                app.world()
                    .resource::<Assets<Mesh>>()
                    .get(&pending)
                    .unwrap()
                    .indices()
                    .unwrap()
                    .len(),
                3 * TERRAIN_QUADRANT_INDEX_COUNT
            );
            assert!(batch.fallback_entities.iter().all(|&fallback| {
                *app.world().get::<Visibility>(fallback).unwrap() == Visibility::Inherited
            }));
        }
        assert_eq!(
            *app.world().get::<Visibility>(entity).unwrap(),
            Visibility::Hidden
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            3
        );
        app.update();
        assert_eq!(
            app.world().get::<Mesh3d>(entity).unwrap().0.id(),
            pending.id(),
            "unchanged selection reuses its pending handle"
        );
        // A second handoff supersedes the still-delayed three-quadrant mask. Its old acknowledgment
        // must not activate stale geometry, even if the renderer was preparing it concurrently.
        app.world_mut()
            .entity_mut(full_detail)
            .remove::<TerrainSurfaceReady>();
        app.update();
        let latest = app.world().get::<Mesh3d>(entity).unwrap().0.clone();
        assert_ne!(latest.id(), pending.id());
        assert!(
            !app.world()
                .resource::<TerrainMeshUploadReadiness>()
                .acknowledge(pending.id())
        );
        assert!(
            app.world()
                .resource::<Assets<Mesh>>()
                .get(&pending)
                .is_none()
        );
        assert_eq!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .batches[0]
                .fallback_entities
                .len(),
            4
        );
        acknowledge_uploads(&app, fixture.root);
        app.update();
        let batch = &app
            .world()
            .get::<LodTerrainBatches>(fixture.root)
            .unwrap()
            .batches[0];
        assert!(batch.fallback_entities.is_empty());
        assert_eq!(
            *app.world().get::<Visibility>(entity).unwrap(),
            Visibility::Inherited
        );
        assert_eq!(
            app.world().get::<Mesh3d>(entity).unwrap().0.id(),
            latest.id()
        );
        app.world_mut()
            .entity_mut(fixture.root)
            .remove::<TerrainSurfaceReady>();
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(entity).unwrap(),
            Visibility::Hidden
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            0
        );
        assert!(
            app.world()
                .get::<LodTerrainBatches>(fixture.root)
                .unwrap()
                .batches[0]
                .fallback_entities
                .is_empty()
        );
    }

    #[test]
    fn flattening_preserves_the_axis_transform_and_origin_rebasing() {
        let mut app = test_app();
        let fixture = fixture(&mut app, &[IVec2::new(-1, 0)], LodTier::Tier4);
        let before = *app.world().get::<Transform>(fixture.scene_root).unwrap();
        let root_transform = *app.world().get::<Transform>(fixture.root).unwrap();
        let prepared = prepare(&mut app, &fixture).unwrap();
        assert_eq!(prepared[0].state.transform, before);
        install_fixture(&mut app, &fixture, prepared);
        let batches = app.world().get::<LodTerrainBatches>(fixture.root).unwrap();
        let entity = batches.batches[0].entity;
        let batch_transform = *app.world().get::<Transform>(entity).unwrap();
        let position = Vec3::new(-CELL_SIZE, 10.0, 20.0);
        assert_eq!(
            root_transform.transform_point(before.transform_point(position)),
            root_transform.transform_point(batch_transform.transform_point(position))
        );
        let shifted = Transform::from_translation(
            root_transform.translation - Vec3::new(CELL_SIZE, 0.0, -CELL_SIZE),
        );
        app.world_mut().entity_mut(fixture.root).insert(shifted);
        assert_eq!(
            shifted.transform_point(batch_transform.transform_point(position)),
            root_transform.transform_point(before.transform_point(position))
                - Vec3::new(CELL_SIZE, 0.0, -CELL_SIZE)
        );
        assert_eq!(
            app.world().get::<ChildOf>(entity).unwrap().parent(),
            fixture.root
        );
    }

    #[test]
    fn independent_roots_keep_source_assets_and_clean_up_only_their_generated_meshes() {
        let mut app = test_app();
        let first = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier4);
        let mut second = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier8);
        // Separate streaming roots can instance the same loaded GLB. Neither conversion may
        // mutate that shared geometry, and retiring one must leave the other intact.
        second.source = first.source.clone();
        app.world_mut()
            .entity_mut(second.root)
            .insert(WorldAssetRoot(second.source.clone()));
        for (&primitive, source) in second.primitives.iter().zip(&first.mesh_handles) {
            app.world_mut()
                .entity_mut(primitive)
                .insert(Mesh3d(source.clone()));
        }
        second.mesh_handles = first.mesh_handles.clone();
        let prepared = prepare(&mut app, &first).unwrap();
        install_fixture(&mut app, &first, prepared);
        let prepared = prepare(&mut app, &second).unwrap();
        install_fixture(&mut app, &second, prepared);
        let first_mesh = app
            .world()
            .get::<LodTerrainBatches>(first.root)
            .unwrap()
            .batches[0]
            .mesh
            .clone();
        let second_mesh = app
            .world()
            .get::<LodTerrainBatches>(second.root)
            .unwrap()
            .batches[0]
            .mesh
            .clone();
        assert_ne!(first_mesh.id(), second_mesh.id());
        assert_eq!(
            app.world()
                .get::<LodTerrainBatches>(first.root)
                .unwrap()
                ._source
                .id(),
            first.source.id()
        );
        app.world_mut().entity_mut(first.root).despawn();
        let meshes = app.world().resource::<Assets<Mesh>>();
        assert!(
            meshes.get(&first_mesh).is_none(),
            "generated mesh cleanup is immediate, even with a retained test handle"
        );
        assert!(meshes.get(&second_mesh).is_some());
        for source in &first.mesh_handles {
            assert!(meshes.get(source).is_some());
        }
        assert!(app.world().get::<LodTerrainBatches>(second.root).is_some());
        app.world_mut().entity_mut(second.root).despawn();
        assert!(
            app.world()
                .resource::<Assets<Mesh>>()
                .get(&second_mesh)
                .is_none()
        );
    }

    #[test]
    fn explicit_selected_bounds_survive_gpu_extraction_and_automatic_bounds_refresh() {
        let mut app = test_app();
        app.add_plugins(bevy::app::TaskPoolPlugin::default());
        let fixture = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier4);
        let prepared = prepare(&mut app, &fixture).unwrap();
        let expected = prepared[0].members[0].bounds;
        install_fixture(&mut app, &fixture, prepared);
        app.world_mut()
            .run_system_once(|mut batches: LodBatchVisibility| {
                batches.update(&HashMap::from([((IVec2::ZERO, 0), Some(LodTier::Tier4))]));
            })
            .unwrap();
        let (entity, handle) = {
            let batches = app.world().get::<LodTerrainBatches>(fixture.root).unwrap();
            (batches.batches[0].entity, batches.batches[0].mesh.clone())
        };
        {
            let mut meshes = app.world_mut().resource_mut::<Assets<Mesh>>();
            let _gpu_copy = meshes.get_mut(&handle).unwrap().take_gpu_data().unwrap();
            let whole_mesh_bounds: Aabb = meshes.get(&handle).unwrap().final_aabb.unwrap().into();
            assert!(whole_mesh_bounds.max().y > expected.max().y);
        }
        acknowledge_uploads(&app, fixture.root);
        assert!(finalize_initial(&mut app));
        assert!(app.world().get::<NoAutoAabb>(entity).is_some());
        assert_eq!(*app.world().get::<Aabb>(entity).unwrap(), expected);
        app.world_mut()
            .run_system_once(bevy::camera::visibility::calculate_bounds)
            .unwrap();
        assert_eq!(*app.world().get::<Aabb>(entity).unwrap(), expected);
    }

    #[test]
    fn ready_chunk_conversion_obeys_the_shared_count_and_elapsed_budget() {
        use super::super::{LodChunkRoot, batch_ready_lod_chunks};
        use crate::streaming::StreamingCommitBudget;
        use shared::lod::{ChunkAnchor, ChunkKey, LodOrigin};
        let mut app = test_app();
        let config = EngineConfig {
            max_commit_micros_per_frame: 1_000_000,
            ..default()
        };
        app.insert_resource(config)
            .init_resource::<StreamingCommitBudget>()
            .init_resource::<LodStreaming>()
            .init_resource::<ProfilingState>();
        let first = fixture(&mut app, &[IVec2::ZERO], LodTier::Tier4);
        let second = fixture(&mut app, &[IVec2::new(4, 0)], LodTier::Tier4);
        for (anchor, fixture) in [(0, &first), (1, &second)] {
            app.world_mut().entity_mut(fixture.root).insert((
                LodChunkRoot {
                    key: ChunkKey::new(1, LodTier::Tier4, ChunkAnchor::new(anchor, 0)),
                    generation: 1,
                    origin: LodOrigin::new(0, 0),
                    retry_count: 0,
                },
                PendingLodTerrainBatching {
                    source: fixture.source.clone(),
                    patches: fixture.patches.clone(),
                },
                Visibility::Inherited,
            ));
            for &(patch, coverage) in &fixture.patches {
                app.world_mut()
                    .entity_mut(patch)
                    .insert((coverage, TerrainSurfaceReady));
            }
        }
        {
            let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
            budget.remaining = 1;
            budget.frame_started = std::time::Instant::now() - std::time::Duration::from_secs(2);
        }
        app.world_mut()
            .run_system_once(batch_ready_lod_chunks)
            .unwrap();
        assert!(app.world().get::<LodTerrainBatches>(first.root).is_none());
        assert!(app.world().get::<LodTerrainBatches>(second.root).is_none());
        app.world_mut()
            .resource_mut::<StreamingCommitBudget>()
            .frame_started = std::time::Instant::now();
        app.world_mut()
            .run_system_once(batch_ready_lod_chunks)
            .unwrap();
        assert!(app.world().get::<LodTerrainBatches>(first.root).is_some());
        assert!(app.world().get::<LodTerrainBatches>(second.root).is_none());
        assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 0);
        assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
        assert!(
            first
                .primitives
                .iter()
                .all(|&entity| app.world().get_entity(entity).is_ok()),
            "upload waits preserve original drawable terrain"
        );
        app.world_mut()
            .run_system_once(batch_ready_lod_chunks)
            .unwrap();
        assert!(
            app.world().get::<LodTerrainBatches>(second.root).is_none(),
            "exhausted count budget cannot convert another ready chunk"
        );
        {
            let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
            budget.remaining = 1;
            budget.commits = 0;
            budget.frame_started = std::time::Instant::now();
        }
        app.world_mut()
            .run_system_once(batch_ready_lod_chunks)
            .unwrap();
        assert!(
            app.world().get::<LodTerrainBatches>(second.root).is_some(),
            "waiting source GPU buffers keep originals; the next preparation can use the available commit"
        );
        assert!(
            app.world()
                .get::<LodTerrainBatches>(first.root)
                .unwrap()
                .source_children
                .is_some()
        );
        assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
    }
}
