//! Re-express temporal snapshots when streaming changes the render coordinate frame.

use super::{
    ActiveSpace, OriginParticipantFilter, RenderOrigin, grid_translation, lod::LodChunkGridOrigin,
};
use crate::{
    render::WaterReflectionCamera,
    world::components::{ExteriorCellGrid, StreamingCamera},
};
use bevy::{
    camera::visibility::VisibilitySystems,
    core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, PreviousViewData},
    ecs::{entity::EntityHashMap, entity_disabling::Disabled, query::Allow, system::SystemParam},
    math::Vec3A,
    pbr::{PreviousGlobalTransform, update_mesh_previous_global_transforms},
    prelude::*,
    render::{occlusion_culling::OcclusionCulling, view::NoIndirectDrawing},
};

mod gpu;
#[cfg(debug_assertions)]
mod origin_guard;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

#[derive(Resource, Default)]
struct CapturedOrigin {
    grid: IVec2,
    space: Option<ActiveSpace>,
}

// Unlike exterior roots, explicitly shifted participants do not follow a door's
// origin assignment. Record only the displacement their mover actually applies.
#[derive(Resource, Default)]
pub(super) struct ParticipantRebase(pub Vec3);

#[derive(Resource, Default)]
struct PendingPreviousMeshes(Vec<Entity>);

pub(super) fn install(app: &mut App) {
    app.init_resource::<CapturedOrigin>()
        .init_resource::<ParticipantRebase>()
        .init_resource::<PendingPreviousMeshes>()
        .add_systems(PreUpdate, capture_origin)
        .add_systems(
            PreUpdate,
            capture_rebased_meshes.before(update_mesh_previous_global_transforms),
        )
        .add_systems(
            PostUpdate,
            rebase_previous_frame_data.after(VisibilitySystems::CheckVisibility),
        );
    #[cfg(debug_assertions)]
    origin_guard::install(app);
    gpu::install(app);
    #[cfg(test)]
    app.init_resource::<TestSnapshot>();
}

// PreviousViewData is installed on every Camera3d, including views that never
// consume history. TAA installs motion vectors; occlusion also requires depth.
// Model and skin history feeds motion vectors. Occlusion additionally consumes
// previous views and retained GPU mesh inputs, but never previous skin matrices.
type OcclusionHistoryViews<'w, 's> = Query<
    'w,
    's,
    &'static Camera,
    (
        With<Camera3d>,
        With<DepthPrepass>,
        With<OcclusionCulling>,
        Without<NoIndirectDrawing>,
    ),
>;

#[derive(SystemParam)]
struct TemporalConsumers<'w, 's> {
    motion: Query<'w, 's, &'static Camera, (With<Camera3d>, With<MotionVectorPrepass>)>,
    occlusion: OcclusionHistoryViews<'w, 's>,
}

impl TemporalConsumers<'_, '_> {
    fn motion(&self) -> bool {
        self.motion.iter().any(|camera| camera.is_active)
    }

    fn occlusion(&self) -> bool {
        self.occlusion.iter().any(|camera| camera.is_active)
    }

    fn mesh_and_view(&self) -> bool {
        self.motion() || self.occlusion()
    }
}

fn capture_rebased_meshes(
    mut pending: ResMut<PendingPreviousMeshes>,
    mut meshes: Query<(&GlobalTransform, &mut PreviousGlobalTransform)>,
) {
    if pending.0.is_empty() {
        return;
    }
    // A late history write must not suppress Bevy's next snapshot. Capture only
    // the queued meshes, including equal flat affines and age-saturated ticks.
    // Ordinary PGT mutation here is intentional: native GPU extraction consumes
    // Changed<PreviousGlobalTransform> to clear last frame's motion history.
    for entity in pending.0.drain(..) {
        if let Ok((global, mut previous)) = meshes.get_mut(entity) {
            *previous = PreviousGlobalTransform(global.affine());
        }
    }
}

// Prepass snapshots are taken in PreUpdate, before any Update origin writer.
fn capture_origin(
    origin: Res<RenderOrigin>,
    active: Option<Res<ActiveSpace>>,
    mut captured: ResMut<CapturedOrigin>,
    mut participants: ResMut<ParticipantRebase>,
) {
    captured.grid = origin.0;
    captured.space = active.as_deref().copied();
    participants.0 = Vec3::ZERO;
}

type ExteriorRoots = (
    Allow<Disabled>,
    Or<(With<ExteriorCellGrid>, With<LodChunkGridOrigin>)>,
);

/// Coordinate ownership, independent of how far an object moved this frame.
/// Disabled ancestors stop ordinary propagation even when a detached draw remains active.
#[derive(SystemParam)]
struct OriginOwnership<'w, 's> {
    disabled: Query<'w, 's, (), With<Disabled>>,
    parents: Query<'w, 's, &'static ChildOf, Allow<Disabled>>,
    roots: Query<'w, 's, (), ExteriorRoots>,
    cameras: Query<'w, 's, (), (Allow<Disabled>, With<StreamingCamera>)>,
    participants: Query<'w, 's, (), (Allow<Disabled>, OriginParticipantFilter)>,
    active: Option<Res<'w, ActiveSpace>>,
}

/// Resolved ancestry is valid only for one crossing. Keep capacity between
/// crossings, and share parent results without caching every mesh leaf.
#[derive(Default)]
struct OwnershipMemo {
    ancestors: EntityHashMap<Option<Vec3>>,
    path: Vec<Entity>,
    #[cfg(test)]
    visits: u64,
    #[cfg(test)]
    hits: u64,
}

impl OwnershipMemo {
    fn clear(&mut self) {
        self.ancestors.clear();
        self.path.clear();
        #[cfg(test)]
        {
            self.visits = 0;
            self.hits = 0;
        }
    }
}

impl OriginOwnership<'_, '_> {
    fn can_rebase(&self, captured: &CapturedOrigin) -> bool {
        captured.space == self.active.as_deref().copied()
            && self
                .active
                .as_ref()
                .is_none_or(|active| active.interior.is_none())
    }

    fn displacement(
        &self,
        mut entity: Entity,
        origin: Vec3,
        participants: Vec3,
        memo: &mut OwnershipMemo,
    ) -> Option<Vec3> {
        memo.path.clear();
        let mut leaf = true;
        let result = loop {
            // The mesh leaf still needs its own disabled/participant/owner
            // checks. Cache its parent and all higher nodes for sibling draws.
            if !leaf {
                if let Some(&displacement) = memo.ancestors.get(&entity) {
                    #[cfg(test)]
                    {
                        memo.hits += 1;
                    }
                    break displacement;
                }
                memo.path.push(entity);
            }
            #[cfg(test)]
            {
                memo.visits += 1;
            }
            if self.disabled.contains(entity) {
                break None;
            }
            // Stop at an owner even when its own coordinate frame did not move.
            if self.participants.contains(entity) {
                break (participants != Vec3::ZERO).then_some(participants);
            }
            if self.roots.contains(entity) || self.cameras.contains(entity) {
                break (origin != Vec3::ZERO).then_some(origin);
            }
            let Some(parent) = self.parents.get(entity).ok().map(ChildOf::parent) else {
                break None;
            };
            entity = parent;
            leaf = false;
        };
        for ancestor in memo.path.drain(..) {
            memo.ancestors.insert(ancestor, result);
        }
        result
    }
}

#[cfg(test)]
type PreviousViews<'w, 's> = Query<
    'w,
    's,
    (
        &'static GlobalTransform,
        &'static mut PreviousViewData,
        Has<StreamingCamera>,
    ),
    Or<(With<StreamingCamera>, With<WaterReflectionCamera>)>,
>;
#[cfg(not(test))]
type PreviousViews<'w, 's> = Query<
    'w,
    's,
    &'static mut PreviousViewData,
    (
        With<GlobalTransform>,
        Or<(With<StreamingCamera>, With<WaterReflectionCamera>)>,
    ),
>;

type PreviousModels<'w, 's> =
    Query<'w, 's, (Entity, &'static mut PreviousGlobalTransform), With<GlobalTransform>>;

#[expect(
    clippy::too_many_arguments,
    reason = "Snapshot repair needs ownership, history, and crossing-local memoization"
)]
fn rebase_previous_frame_data(
    origin: Res<RenderOrigin>,
    captured: Res<CapturedOrigin>,
    participants: Res<ParticipantRebase>,
    ownership: OriginOwnership,
    consumers: TemporalConsumers,
    mut views: PreviousViews,
    mut models: PreviousModels,
    mut pending: ResMut<PendingPreviousMeshes>,
    #[cfg(test)] mut snapshot: ResMut<TestSnapshot>,
    mut memo: Local<OwnershipMemo>,
    #[cfg(debug_assertions)] sealed: Option<Res<origin_guard::SealedOrigin>>,
) {
    #[cfg(debug_assertions)]
    origin_guard::assert_origin(&origin, sealed.as_deref());
    if captured.grid == origin.0 {
        return;
    }
    let motion = consumers.motion();
    if !motion && !consumers.occlusion() {
        return;
    }
    memo.clear();
    let displacement = grid_translation(origin.0 - captured.grid, IVec2::ZERO);
    let compensate = ownership.can_rebase(&captured);
    let to_old_frame = Mat4::from_translation(displacement);
    let to_new_frame = Mat4::from_translation(-displacement);
    for view in &mut views {
        #[cfg(test)]
        let (now, mut previous, main_view) = view;
        #[cfg(not(test))]
        let mut previous = view;
        let previous = previous.bypass_change_detection();
        #[cfg(test)]
        let (before, frame_motion) = {
            let old_camera = previous.view_from_world.inverse().w_axis.truncate();
            (
                (old_camera - now.translation()).length(),
                (old_camera - displacement - now.translation()).length(),
            )
        };
        if compensate {
            // New render coordinates p map to old coordinates p + displacement.
            // Projection-only matrices stay camera-local, including old jitter.
            previous.view_from_world *= to_old_frame;
            previous.clip_from_world *= to_old_frame;
            previous.world_from_clip = to_new_frame * previous.world_from_clip;
        }
        // A reflection shares the main camera's frame and uses previous views
        // for occlusion even without TAA. Space changes remain real cuts.
        #[cfg(test)]
        if main_view {
            let previous_camera = previous.view_from_world.inverse().w_axis.truncate();
            snapshot.camera_delta_before = Some(f64::from(before));
            snapshot.camera_delta = Some(f64::from((previous_camera - now.translation()).length()));
            snapshot.camera_frame_motion = Some(f64::from(frame_motion));
        }
    }
    if compensate && motion {
        for (entity, mut previous) in &mut models {
            let Some(shift) =
                ownership.displacement(entity, displacement, participants.0, &mut memo)
            else {
                continue;
            };
            // Preserve current-global provenance and the native snapshot tick.
            // Explicit renderer admission and a one-shot next-frame snapshot
            // handle equal flat affines without marking current transforms.
            previous.bypass_change_detection().0.translation -= Vec3A::from(shift);
            pending.0.push(entity);
        }
    }
}

/// Per-World observations for the original crossing assertions, absent in production.
#[cfg(test)]
#[derive(Resource, Default)]
struct TestSnapshot {
    camera_delta_before: Option<f64>,
    camera_delta: Option<f64>,
    camera_frame_motion: Option<f64>,
}
