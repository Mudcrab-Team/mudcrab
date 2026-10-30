//! Repair retained GPU history through Bevy's public buffers, without changing current poses.

use super::{
    CapturedOrigin, OriginOwnership, OwnershipMemo, ParticipantRebase, RenderOrigin,
    TemporalConsumers, grid_translation,
};
use bevy::{
    mesh::skinning::SkinnedMesh,
    pbr::{
        MeshExtractionSystems, MeshInputUniform, MeshUniform, MeshesToReextractNextFrame,
        PreviousGlobalTransform, RenderMeshInstances, SkinUniforms, extract_skins, prepare_skins,
    },
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        batching::gpu_preprocessing::BatchedInstanceBuffers, renderer::RenderQueue,
        sync_world::MainEntity,
    },
};

#[derive(Resource, Default)]
struct PreviousSkins(Vec<PreviousSkin>);

struct PreviousSkin {
    entity: MainEntity,
    joints: Vec<Mat4>,
}

pub(super) fn install(app: &mut App) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    #[cfg(debug_assertions)]
    render_app.add_systems(
        ExtractSchedule,
        assert_origin_on_extract
            .before(MeshExtractionSystems)
            .before(extract_skins),
    );
    render_app
        .init_resource::<PreviousSkins>()
        .add_systems(
            ExtractSchedule,
            (
                rebase_gpu_mesh_history.before(MeshExtractionSystems),
                capture_previous_skins.before(extract_skins),
            ),
        )
        .add_systems(
            Render,
            upload_previous_skins
                .in_set(RenderSystems::PrepareResources)
                .after(prepare_skins)
                .before(RenderSystems::PrepareBindGroups),
        );
}

type RetainedMeshes<'w, 's> =
    Query<'w, 's, (Entity, Ref<'static, GlobalTransform>), With<PreviousGlobalTransform>>;

#[expect(
    clippy::too_many_arguments,
    reason = "Retained GPU history and native extraction admission belong to the same pass"
)]
fn rebase_gpu_mesh_history(
    origin: Extract<Res<RenderOrigin>>,
    captured: Extract<Res<CapturedOrigin>>,
    participants: Extract<Res<ParticipantRebase>>,
    ownership: Extract<OriginOwnership>,
    consumers: Extract<TemporalConsumers>,
    meshes: Extract<RetainedMeshes>,
    instances: Res<RenderMeshInstances>,
    buffers: Option<Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>>,
    reextract: Option<ResMut<MeshesToReextractNextFrame>>,
    mut memo: Local<OwnershipMemo>,
) {
    if captured.grid == origin.0 || !ownership.can_rebase(&captured) || !consumers.mesh_and_view() {
        return;
    }
    let displacement = grid_translation(origin.0 - captured.grid, IVec2::ZERO);
    memo.clear();
    let mut missing_rows = 0;
    // CPU building reads the repaired PreviousGlobalTransform directly. GPU
    // building only checks its presence: collection saves old current input rows
    // into a separate previous table before replacing the current rows.
    if matches!(*instances, RenderMeshInstances::GpuBuilding(_)) {
        let (Some(buffers), Some(mut reextract)) = (buffers, reextract) else {
            error!("GPU temporal rebase requires retained buffers and mesh re-extraction requests");
            return;
        };
        for (entity, global) in &meshes {
            let Some(instance) = instances.render_mesh_queue_data(entity.into()) else {
                continue;
            };
            let Some(shift) =
                ownership.displacement(entity, displacement, participants.0, &mut memo)
            else {
                continue;
            };
            let index = instance.current_uniform_index.0;
            if index as usize >= buffers.current_input_buffer.len() {
                missing_rows += 1;
                continue;
            }
            // The retained instance owns this live slot until collection. Bounds
            // are checked above; avoid a linear search through all free slots.
            let mut input = buffers.current_input_buffer.get_unchecked(index);
            shift_mesh_input(&mut input, shift);
            buffers.current_input_buffer.set(index, input);
            // Collection must replace the patched current slot even when an
            // equal flat pose left GlobalTransform unchanged. Preserve existing
            // material-readiness retry requests in this native admission set.
            if !global.is_changed() {
                reextract.insert(entity.into());
            }
        }
    }
    if missing_rows != 0 {
        error!(
            missing_rows,
            "Retained meshes have no input row during temporal rebase"
        );
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Native skin capture needs frame ownership, retained buffers, and crossing-local memoization"
)]
fn capture_previous_skins(
    origin: Extract<Res<RenderOrigin>>,
    captured: Extract<Res<CapturedOrigin>>,
    participants: Extract<Res<ParticipantRebase>>,
    ownership: Extract<OriginOwnership>,
    consumers: Extract<TemporalConsumers>,
    skins: Extract<Query<(Entity, Ref<SkinnedMesh>)>>,
    uniforms: Res<SkinUniforms>,
    mut previous_skins: ResMut<PreviousSkins>,
    mut memo: Local<OwnershipMemo>,
) {
    if captured.grid == origin.0 || !ownership.can_rebase(&captured) || !consumers.motion() {
        return;
    }
    let displacement = grid_translation(origin.0 - captured.grid, IVec2::ZERO);
    memo.clear();
    let mut missing_rows = 0;
    for (entity, skin) in &skins {
        let Some(index) = uniforms.skin_index(entity.into()) else {
            continue;
        };
        // Replaced joint topology has no proven vertex correspondence. Keep
        // Bevy's native new-skin policy; do not map old rows to new joint IDs.
        if skin.is_changed() {
            continue;
        }
        let start = index as usize;
        let Some(old) = uniforms
            .current_staging_buffer
            .get(start..start + skin.joints.len())
        else {
            missing_rows += 1;
            continue;
        };
        if let Some(joints) = shifted_joints(
            &skin,
            old,
            &ownership,
            displacement,
            participants.0,
            &mut memo,
        ) {
            previous_skins.0.push(PreviousSkin {
                entity: entity.into(),
                joints,
            });
        }
    }
    if missing_rows != 0 {
        error!(
            missing_rows,
            "Retained skins have no prior joint rows during temporal rebase"
        );
    }
}

fn shifted_joints(
    skin: &SkinnedMesh,
    old: &[Mat4],
    ownership: &OriginOwnership,
    displacement: Vec3,
    participants: Vec3,
    memo: &mut OwnershipMemo,
) -> Option<Vec<Mat4>> {
    let mut shifted = false;
    let joints = skin
        .joints
        .iter()
        .zip(old)
        .map(|(&joint, &matrix)| {
            if let Some(shift) = ownership.displacement(joint, displacement, participants, memo) {
                shifted = true;
                Mat4::from_translation(-shift) * matrix
            } else {
                matrix
            }
        })
        .collect();
    shifted.then_some(joints)
}

fn shift_mesh_input(input: &mut MeshInputUniform, displacement: Vec3) {
    input.world_from_local[0].w -= displacement.x;
    input.world_from_local[1].w -= displacement.y;
    input.world_from_local[2].w -= displacement.z;
}

fn upload_previous_skins(
    mut previous: ResMut<PreviousSkins>,
    uniforms: Res<SkinUniforms>,
    queue: Res<RenderQueue>,
) {
    // Resolve final allocations after extract_skins and upload after buffer
    // swapping/growth. Current skin poses and staging data remain untouched.
    for skin in previous.0.drain(..) {
        let Some(index) = uniforms.skin_index(skin.entity) else {
            continue;
        };
        let offset = u64::from(index) * std::mem::size_of::<Mat4>() as u64;
        queue.write_buffer(
            &uniforms.prev_buffer,
            offset,
            bytemuck::cast_slice(&skin.joints),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::components::{CELL_SIZE, ExteriorCellGrid};
    use bevy::{ecs::system::SystemState, render::MainWorld};

    #[test]
    fn cpu_mesh_building_needs_no_gpu_input_buffer() {
        let mut main = MainWorld::default();
        main.insert_resource(RenderOrigin(IVec2::X));
        main.insert_resource(CapturedOrigin::default());
        main.insert_resource(ParticipantRebase::default());
        main.spawn((
            Camera3d::default(),
            Camera::default(),
            bevy::core_pipeline::prepass::MotionVectorPrepass,
        ));
        let mut render = World::new();
        render.insert_resource(main);
        render.insert_resource(RenderMeshInstances::CpuBuilding(default()));
        let mut extract = Schedule::new(ExtractSchedule);
        extract.add_systems(rebase_gpu_mesh_history);
        extract.run(&mut render);
        assert!(
            !render.contains_resource::<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>()
        );
    }

    #[test]
    fn previous_skin_joints_preserve_animation_and_absolute_interior_poses() {
        let mut world = World::new();
        let root = world.spawn(ExteriorCellGrid(IVec2::ZERO)).id();
        let exterior = world.spawn(ChildOf(root)).id();
        let interior = world.spawn_empty().id();
        let skin = SkinnedMesh {
            inverse_bindposes: default(),
            joints: vec![exterior, interior],
        };
        let old = [
            Mat4::from_scale_rotation_translation(
                Vec3::new(2.0, 3.0, 4.0),
                Quat::from_rotation_y(0.6),
                Vec3::new(30.0, 40.0, -90.0),
            ),
            Mat4::from_translation(Vec3::new(2.0, 3.0, 4.0)),
        ];
        let mut state = SystemState::<OriginOwnership>::new(&mut world);
        let ownership = state.get(&world).unwrap();
        let displacement = Vec3::new(CELL_SIZE, 0.0, -CELL_SIZE);
        let shifted = shifted_joints(
            &skin,
            &old,
            &ownership,
            displacement,
            Vec3::ZERO,
            &mut OwnershipMemo::default(),
        )
        .unwrap();
        let vertex = Vec4::new(1.0, 2.0, 3.0, 1.0);
        let old_position = old[0] * vertex;
        let previous_position = shifted[0] * vertex;
        assert!(
            (previous_position.truncate() + displacement)
                .abs_diff_eq(old_position.truncate(), 0.001)
        );
        assert_eq!(shifted[1], old[1]);
        // An actual joint rotation/translation remains animation, rather than
        // replacing previous joints with the current pose to hide the rebase.
        let animated =
            Mat4::from_translation(Vec3::new(3.0, 1.0, 2.0)) * Mat4::from_rotation_y(0.2) * old[0];
        let current = Mat4::from_translation(-displacement) * animated;
        let actual_motion = current * vertex - previous_position;
        let expected_motion = animated * vertex - old_position;
        assert!(actual_motion.abs_diff_eq(expected_motion, 0.001));
        assert_ne!(shifted[0], current);
    }
}

#[cfg(debug_assertions)]
fn assert_origin_on_extract(
    origin: Extract<Res<RenderOrigin>>,
    sealed: Extract<Option<Res<super::origin_guard::SealedOrigin>>>,
) {
    super::origin_guard::assert_origin(&origin, sealed.as_deref());
}

#[cfg(test)]
#[path = "gpu_skin_tests.rs"]
mod skin_tests;
