//! Opt-in, bounded comparison of cached mesh inputs with current GPU allocations.
//!
//! This reads CPU submission metadata, not GPU execution or rendered pixels. The optional repair
//! requests Bevy's existing extraction path for only resident instances whose cached inputs differ.

use bevy::{
    pbr::{MeshInputUniform, MeshUniform, MeshesToReextractNextFrame, RenderMeshInstances},
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        batching::gpu_preprocessing::BatchedInstanceBuffers,
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
    },
};
use std::{ops::Range, time::Instant};

const DEFAULT_AUDIT_FRAME: u64 = 600;
const MAX_SAMPLES: usize = 16;

pub struct MeshResidencyAuditPlugin;

impl Plugin for MeshResidencyAuditPlugin {
    fn build(&self, app: &mut App) {
        let repair = std::env::var("MUDCRAB_REPAIR_MESH_RESIDENCY").as_deref() == Ok("1");
        let enabled = repair || std::env::var("MUDCRAB_MESH_RESIDENCY_AUDIT").as_deref() == Ok("1");
        if !enabled {
            return;
        }
        let target_frame = std::env::var("MUDCRAB_MESH_RESIDENCY_AUDIT_FRAME")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|&frame| frame > 0)
            .unwrap_or(DEFAULT_AUDIT_FRAME);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(AuditState {
                    frame: 0,
                    target_frame,
                    repair,
                    followup_frame: None,
                    done: false,
                })
                .add_systems(
                    Render,
                    audit_mesh_residency
                        .after(RenderSystems::PrepareMeshes)
                        .before(RenderSystems::Prepare),
                );
            info!(
                target_frame,
                repair, "mesh residency audit enabled; timing observation is instrumented"
            );
        } else {
            warn!("mesh residency audit unavailable: no render app");
        }
    }
}

#[derive(Resource)]
struct AuditState {
    frame: u64,
    target_frame: u64,
    repair: bool,
    followup_frame: Option<u64>,
    done: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DrawMetadata {
    first_vertex_index: u32,
    first_index_index: Option<u32>,
    count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unavailable {
    VertexRange,
    IndexRange,
}

// Match Bevy's input-builder counts, which come from allocator ranges. Check the prepared
// descriptor's logical counts separately so an incomplete allocation cannot become a repair.
fn expected_metadata(
    vertex_count: u32,
    indexed_count: Option<u32>,
    vertex_range: Option<Range<u32>>,
    index_range: Option<Range<u32>>,
) -> Result<Option<DrawMetadata>, Unavailable> {
    let vertex_range = vertex_range.ok_or(Unavailable::VertexRange)?;
    let allocated_vertices = vertex_range
        .end
        .checked_sub(vertex_range.start)
        .filter(|&count| count >= vertex_count)
        .ok_or(Unavailable::VertexRange)?;
    let (first_index_index, count) = match indexed_count {
        // Hidden, empty indexed batches have no draw geometry. Bevy may use the vertex count
        // when an empty index allocation is absent; that is not a missing terrain draw.
        Some(0) => return Ok(None),
        Some(logical_indices) => {
            let range = index_range.ok_or(Unavailable::IndexRange)?;
            let count = range
                .end
                .checked_sub(range.start)
                .filter(|&count| count >= logical_indices)
                .ok_or(Unavailable::IndexRange)?;
            (Some(range.start), count)
        }
        None => (None, allocated_vertices),
    };
    Ok(Some(DrawMetadata {
        first_vertex_index: vertex_range.start,
        first_index_index,
        count,
    }))
}

#[derive(Default)]
struct AuditCounts {
    instances: usize,
    descriptor_missing: usize,
    vertex_unavailable: usize,
    index_unavailable: usize,
    empty_indexed: usize,
    resident: usize,
    uniform_missing: usize,
    vertex_offset_mismatch: usize,
    index_offset_mismatch: usize,
    count_mismatch: usize,
    mismatched_instances: usize,
    repair_queued: usize,
}

fn audit_mesh_residency(
    mut state: ResMut<AuditState>,
    instances: Option<Res<RenderMeshInstances>>,
    buffers: Option<Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>>,
    meshes: Option<Res<RenderAssets<RenderMesh>>>,
    allocator: Option<Res<MeshAllocator>>,
    mut retry: Option<ResMut<MeshesToReextractNextFrame>>,
) {
    if state.done {
        return;
    }
    state.frame += 1;
    let followup = state.followup_frame.is_some();
    let target = state.followup_frame.unwrap_or(state.target_frame);
    if state.frame < target {
        return;
    }
    let (Some(instances), Some(buffers), Some(meshes), Some(allocator)) =
        (instances, buffers, meshes, allocator)
    else {
        warn!(
            render_frame = state.frame,
            followup, "mesh residency audit unavailable: missing render resources"
        );
        state.done = true;
        return;
    };
    let RenderMeshInstances::GpuBuilding(gpu_instances) = &*instances else {
        warn!(
            render_frame = state.frame,
            "mesh residency audit unavailable: CPU mesh inputs in use"
        );
        state.done = true;
        return;
    };

    let started = Instant::now();
    let mut counts = AuditCounts::default();
    let mut samples = 0;
    for &entity in gpu_instances.keys() {
        counts.instances += 1;
        let Some(asset_id) = instances.mesh_asset_id(entity) else {
            continue;
        };
        let Some(mesh) = meshes.get(asset_id) else {
            counts.descriptor_missing += 1;
            continue;
        };
        let indexed_count = match mesh.buffer_info {
            RenderMeshBufferInfo::Indexed { count, .. } => Some(count),
            RenderMeshBufferInfo::NonIndexed => None,
        };
        let vertex_range = allocator
            .mesh_vertex_slice(&asset_id)
            .map(|slice| slice.range);
        let index_range = allocator
            .mesh_index_slice(&asset_id)
            .map(|slice| slice.range);
        let expected = match expected_metadata(
            mesh.vertex_count,
            indexed_count,
            vertex_range.clone(),
            index_range.clone(),
        ) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                counts.empty_indexed += 1;
                continue;
            }
            Err(Unavailable::VertexRange) => {
                counts.vertex_unavailable += 1;
                continue;
            }
            Err(Unavailable::IndexRange) => {
                counts.index_unavailable += 1;
                continue;
            }
        };
        counts.resident += 1;
        let uniform_index = instances
            .render_mesh_queue_data(entity)
            .map(|data| data.current_uniform_index.0);
        let input = uniform_index.and_then(|index| buffers.current_input_buffer.get(index));
        let mismatch = match input {
            Some(input) => {
                let vertex = input.first_vertex_index != expected.first_vertex_index;
                let index = expected
                    .first_index_index
                    .is_some_and(|expected| input.first_index_index != expected);
                let count = input.index_count != expected.count;
                counts.vertex_offset_mismatch += usize::from(vertex);
                counts.index_offset_mismatch += usize::from(index);
                counts.count_mismatch += usize::from(count);
                vertex || index || count
            }
            None => {
                counts.uniform_missing += 1;
                true
            }
        };
        if !mismatch {
            continue;
        }
        counts.mismatched_instances += 1;
        if state.repair
            && !followup
            && let Some(retry) = retry.as_mut()
        {
            retry.insert(entity);
            counts.repair_queued += 1;
        }
        if samples < MAX_SAMPLES {
            samples += 1;
            warn!(
                render_frame = state.frame,
                followup,
                ?entity,
                ?asset_id,
                ?uniform_index,
                cached = ?input.map(|input| (input.first_vertex_index, input.first_index_index, input.index_count)),
                ?expected,
                ?vertex_range,
                ?index_range,
                descriptor_vertices = mesh.vertex_count,
                ?indexed_count,
                "mesh residency audit mismatch"
            );
        }
    }
    info!(
        render_frame = state.frame,
        followup,
        repair_enabled = state.repair,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        instances = counts.instances,
        descriptor_missing = counts.descriptor_missing,
        vertex_unavailable = counts.vertex_unavailable,
        index_unavailable = counts.index_unavailable,
        empty_indexed = counts.empty_indexed,
        resident = counts.resident,
        uniform_missing = counts.uniform_missing,
        vertex_offset_mismatch = counts.vertex_offset_mismatch,
        index_offset_mismatch = counts.index_offset_mismatch,
        count_mismatch = counts.count_mismatch,
        mismatched_instances = counts.mismatched_instances,
        repair_queued = counts.repair_queued,
        sample_limit = MAX_SAMPLES,
        "mesh residency audit complete"
    );
    if !followup && counts.repair_queued > 0 {
        state.followup_frame = Some(state.frame + 2);
    } else {
        state.done = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_indexed_draw_uses_current_allocator_offsets_and_counts() {
        assert_eq!(
            expected_metadata(4, Some(6), Some(20..24), Some(40..46)),
            Ok(Some(DrawMetadata {
                first_vertex_index: 20,
                first_index_index: Some(40),
                count: 6,
            }))
        );
        assert_eq!(
            expected_metadata(4, Some(6), Some(20..25), Some(40..47)),
            Ok(Some(DrawMetadata {
                first_vertex_index: 20,
                first_index_index: Some(40),
                count: 7,
            }))
        );
    }

    #[test]
    fn unallocated_or_incomplete_geometry_cannot_be_repaired_as_resident() {
        assert_eq!(
            expected_metadata(4, Some(6), None, Some(40..46)),
            Err(Unavailable::VertexRange)
        );
        for vertices in [20..23, Range { start: 24, end: 20 }] {
            assert_eq!(
                expected_metadata(4, Some(6), Some(vertices), Some(40..46)),
                Err(Unavailable::VertexRange)
            );
        }
        for indices in [None, Some(40..45), Some(Range { start: 46, end: 40 })] {
            assert_eq!(
                expected_metadata(4, Some(6), Some(20..24), indices),
                Err(Unavailable::IndexRange)
            );
        }
    }

    #[test]
    fn empty_indexed_draw_is_separate_from_nonindexed_geometry() {
        assert_eq!(expected_metadata(4, Some(0), Some(20..24), None), Ok(None));
        assert_eq!(
            expected_metadata(4, None, Some(20..24), None),
            Ok(Some(DrawMetadata {
                first_vertex_index: 20,
                first_index_index: None,
                count: 4,
            }))
        );
        assert_eq!(
            expected_metadata(4, Some(0), None, None),
            Err(Unavailable::VertexRange)
        );
    }
}
