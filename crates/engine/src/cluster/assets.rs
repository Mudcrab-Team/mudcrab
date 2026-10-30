//! Validated in-memory cluster payloads (ADR-0013 sections 2 and 3).
//!
//! The package reader checks the exact owned bytes and the shared format before publishing
//! these values. Production meshes share transient bytes until upload handoff; small metadata
//! and tangent proof survive release. Explicit in-memory fixtures retain their payloads.

use bevy::{asset::Asset, prelude::*, reflect::TypePath};
use shared::cluster::{
    ClusterFormatError, FLAG_TANGENTS, InstanceRecord, MclSection, MclView, MeshRef, NO_GROUP,
    decode_oct_snorm16, decode_uv_unorm16, encode_oct_snorm16, read_mci, read_mcl,
    read_mcl_validated,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, AtomicU64, Ordering},
};

struct MeshPayload {
    bytes: Arc<[u8]>,
    tangents: Option<PreparedTangents>,
}

struct UploadState {
    payload: Option<MeshPayload>,
    rebuilding: bool,
    failure: Option<String>,
    /// The first worker chooses; later block workers cannot change terrain-only proof.
    terrain_only: Option<bool>,
    /// A replacement scene may request the same mesh while an old request is in flight.
    waiters: Vec<Arc<AtomicU64>>,
}

struct MeshUpload {
    state: Mutex<UploadState>,
    /// 0 pending, 1 success, 2 diagnostic/failure. Independent of resident words.
    proof: AtomicU8,
    #[cfg(test)]
    requests: std::sync::atomic::AtomicUsize,
}

/// Logical CPU allocation estimate, not RSS. Shared upload cells count once.
/// Excludes GPU/staging/driver memory, allocator overhead and the package's indexes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg(test)]
pub(super) struct MeshResidentEstimate {
    pub metadata_bytes: usize,
    pub scene_row_bytes: usize,
    pub raw_payload_bytes: usize,
    pub tangent_bytes: usize,
    pub rehydrating_meshes: usize,
    /// Planned payload/word allocations for outstanding jobs, not measured worker memory.
    pub planned_rehydrate_raw_bytes: usize,
    pub planned_rehydrate_tangent_bytes: usize,
}

struct PreparedTangents {
    words: Arc<[u32]>,
    usable: bool,
}

fn pack_tangent_word(tangent: [f32; 4]) -> u32 {
    let mut encoded = encode_oct_snorm16([tangent[0], tangent[1], tangent[2]]);
    encoded[1] = (encoded[1] & !1) | i16::from(tangent[3] < 0.0);
    u32::from(encoded[0] as u16) | (u32::from(encoded[1] as u16) << 16)
}

/// Preserve successful MikkTSpace words exactly, including zero slots only
/// parent clusters use. Failure words remain diagnostic, never usable proof.
fn prepare_generated_tangents(
    count: usize,
    succeeded: bool,
    attribute: Option<&bevy::mesh::VertexAttributeValues>,
) -> PreparedTangents {
    if succeeded
        && let Some(bevy::mesh::VertexAttributeValues::Float32x4(tangents)) = attribute
        && tangents.len() == count
        && tangents
            .iter()
            .flatten()
            .all(|component| component.is_finite())
    {
        return PreparedTangents {
            words: tangents
                .iter()
                .copied()
                .map(pack_tangent_word)
                .collect::<Vec<_>>()
                .into(),
            usable: true,
        };
    }
    PreparedTangents {
        words: vec![pack_tangent_word([1.0, 0.0, 0.0, 1.0]); count].into(),
        usable: false,
    }
}

/// The same unorm8 RGBA interpretation as the pooled vertex shader, including alpha.
fn source_colours(bytes: &[u8], count: usize) -> Vec<[f32; 4]> {
    if bytes.is_empty() {
        return vec![[1.0; 4]; count];
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|rgba| std::array::from_fn(|channel| f32::from(rgba[channel]) / 255.0))
        .collect()
}

/// A validated per-mesh cluster payload (`.mcl`).
#[derive(Asset, TypePath, Clone)]
pub struct ClusterMeshAsset {
    upload: Arc<MeshUpload>,
    source: Option<Arc<super::mesh_payload::MeshRehydrateSource>>,
    retain_in_memory: bool,
    byte_len: usize,
    section_lengths: [usize; shared::cluster::MCL_SECTION_COUNT],
    pub builder_identity: [u8; 32],
    pub mesh_hash: [u8; 32],
    pub primitive_index: u32,
    pub flags: u32,
    pub vertex_count: u32,
    pub cluster_count: u32,
    pub group_count: u32,
    pub bvh_node_count: u32,
    pub root_error: f32,
    pub bounds: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_scale: [f32; 2],
}

impl ClusterMeshAsset {
    /// Validates `bytes` and keeps them.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ClusterFormatError> {
        let mut mesh = Self::from_shared_bytes(bytes.into())?;
        // Explicitly invented fixtures/examples have no certified reread source.
        mesh.retain_in_memory = true;
        Ok(mesh)
    }

    pub(crate) fn from_shared_bytes(bytes: Arc<[u8]>) -> Result<Self, ClusterFormatError> {
        let view = read_mcl(&bytes)?;
        let asset = Self {
            builder_identity: view.builder_identity,
            mesh_hash: view.mesh_hash,
            primitive_index: view.primitive_index,
            flags: view.flags,
            vertex_count: view.vertex_count,
            cluster_count: view.cluster_count,
            group_count: view.group_count,
            bvh_node_count: view.bvh_node_count,
            root_error: view.root_error,
            bounds: view.bounds,
            uv_min: view.uv_min,
            uv_scale: view.uv_scale,
            byte_len: bytes.len(),
            section_lengths: [
                MclSection::Positions,
                MclSection::Normals,
                MclSection::Uvs,
                MclSection::Tangents,
                MclSection::Colours,
                MclSection::Clusters,
                MclSection::ClusterVertices,
                MclSection::ClusterTriangles,
                MclSection::Groups,
                MclSection::Bvh,
            ]
            .map(|section| view.section(section).len()),
            upload: Arc::new(MeshUpload {
                state: Mutex::new(UploadState {
                    payload: Some(MeshPayload {
                        bytes,
                        tangents: None,
                    }),
                    rebuilding: false,
                    failure: None,
                    terrain_only: None,
                    waiters: Vec::new(),
                }),
                proof: AtomicU8::new(0),
                #[cfg(test)]
                requests: std::sync::atomic::AtomicUsize::new(0),
            }),
            source: None,
            retain_in_memory: false,
        };
        Ok(asset)
    }

    /// An explicitly owned diagnostic snapshot. Production upload uses `with_upload`
    /// instead so workers and extracted clones cannot pin bytes after handoff.
    pub(crate) fn snapshot(&self) -> Result<Arc<[u8]>, String> {
        let state = self
            .upload
            .state
            .lock()
            .map_err(|_| "mesh payload lock poisoned")?;
        state
            .payload
            .as_ref()
            .map(|payload| payload.bytes.clone())
            .ok_or_else(|| "mesh upload payload is not resident".into())
    }

    /// An owned diagnostic section; never borrows beyond the payload guard.
    pub fn section(&self, section: MclSection) -> Result<Vec<u8>, String> {
        let bytes = self.snapshot()?;
        let view = read_mcl_validated(&bytes).map_err(|error| error.to_string())?;
        Ok(view.section(section).to_vec())
    }

    pub(crate) fn section_len(&self, section: MclSection) -> usize {
        self.section_lengths[section as usize]
    }

    pub(crate) fn set_rehydrate_source(
        &mut self,
        source: super::mesh_payload::MeshRehydrateSource,
    ) {
        self.source = Some(Arc::new(source));
    }

    /// The mesh as the stock path sees it: one ordinary indexed triangle list of the source
    /// geometry (the clusters no simplification produced), retaining colours and authored tangents.
    pub fn source_mesh(&self) -> Result<Mesh, String> {
        let bytes = self.snapshot()?;
        let view = read_mcl_validated(&bytes).map_err(|error| error.to_string())?;
        Ok(self.source_mesh_from_view(&view))
    }

    fn source_mesh_from_view(&self, view: &MclView<'_>) -> Mesh {
        let count = self.vertex_count as usize;
        let positions: Vec<[f32; 3]> = (0..count).map(|vertex| view.position(vertex)).collect();
        let normals: Vec<[f32; 3]> = (0..count)
            .map(|vertex| decode_oct_snorm16(view.normal(vertex)))
            .collect();
        let uvs: Vec<[f32; 2]> = (0..count)
            .map(|vertex| decode_uv_unorm16(view.uv(vertex), self.uv_min, self.uv_scale))
            .collect();
        let colours = source_colours(view.section(MclSection::Colours), count);
        let mut indices = Vec::new();
        for index in 0..self.cluster_count as usize {
            let cluster = view.cluster(index);
            if cluster.refined_group != NO_GROUP {
                continue;
            }
            for local in view.cluster_triangles(&cluster) {
                indices.push(view.cluster_vertex(cluster.vertex_offset as usize + *local as usize));
            }
        }
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
        .with_inserted_indices(bevy::mesh::Indices::U32(indices));
        if self.flags & FLAG_TANGENTS != 0 {
            let tangents: Vec<[f32; 4]> = view
                .section(MclSection::Tangents)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| {
                    let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    let direction =
                        decode_oct_snorm16([word as u16 as i16, ((word >> 16) as u16 & !1) as i16]);
                    [
                        direction[0],
                        direction[1],
                        direction[2],
                        if word & 0x10000 != 0 { -1.0 } else { 1.0 },
                    ]
                })
                .collect();
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, tangents);
        }
        mesh
    }

    /// Explicit in-memory callers may retain an owned tangent snapshot. The production
    /// upload path borrows words only while holding the shared payload guard.
    pub fn tangent_words(&self) -> Result<Arc<[u32]>, String> {
        self.prepare_tangents()?;
        let state = self
            .upload
            .state
            .lock()
            .map_err(|_| "mesh payload lock poisoned")?;
        state
            .payload
            .as_ref()
            .and_then(|payload| payload.tangents.as_ref())
            .map(|prepared| prepared.words.clone())
            .ok_or_else(|| "mesh tangent words are not resident".into())
    }

    /// Serialize preparation and handoff per mesh. A late block worker sees persistent
    /// proof after release and never regenerates or holds an independent byte owner.
    pub fn prepare_tangents(&self) -> Result<(), String> {
        self.prepare(false)
    }

    pub(crate) fn prepare_unused_tangent_slots(&self) -> Result<(), String> {
        self.prepare(true)
    }

    fn prepare(&self, terrain_only: bool) -> Result<(), String> {
        let mut state = self
            .upload
            .state
            .lock()
            .map_err(|_| "mesh payload lock poisoned")?;
        if let Some(reason) = &state.failure {
            return Err(reason.clone());
        }
        if self.prepared_tangents_usable().is_some() {
            return Ok(());
        }
        let payload = state
            .payload
            .as_mut()
            .ok_or("mesh payload released before preparation")?;
        let prepared = if terrain_only {
            prepare_generated_tangents(self.vertex_count as usize, false, None)
        } else {
            let view = read_mcl_validated(&payload.bytes).map_err(|error| error.to_string())?;
            self.generate_tangent_words(&view)
        };
        let usable = prepared.usable;
        payload.tangents = Some(prepared);
        state.terrain_only = Some(terrain_only);
        self.upload
            .proof
            .store(if usable { 1 } else { 2 }, Ordering::Release);
        Ok(())
    }

    pub fn tangents_usable(&self) -> bool {
        // Preserve explicit fixture behavior. Certified meshes only read worker proof.
        if self.retain_in_memory && self.prepare_tangents().is_err() {
            return false;
        }
        self.prepared_tangents_usable() == Some(true)
    }

    pub(crate) fn prepared_tangents_usable(&self) -> Option<bool> {
        match self.upload.proof.load(Ordering::Acquire) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// Queue::write_buffer copies the input before returning. The callback must perform
    /// those writes; on return, clear the one shared payload owner under this same guard.
    /// Never generate tangents or wait for a worker on a production render thread.
    pub(crate) fn with_upload<R>(
        &self,
        write: impl FnOnce(&MclView<'_>, &[u32]) -> R,
    ) -> Result<Option<R>, String> {
        if self.retain_in_memory {
            self.prepare_tangents()?;
        }
        let mut state = match self.upload.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("mesh payload lock poisoned".into());
            }
        };
        if let Some(reason) = &state.failure {
            return Err(reason.clone());
        }
        let Some(payload) = state.payload.as_ref() else {
            return Ok(None);
        };
        let Some(tangents) = payload.tangents.as_ref() else {
            return Ok(None);
        };
        let view = read_mcl_validated(&payload.bytes).map_err(|error| error.to_string())?;
        let result = write(&view, &tangents.words);
        if !self.retain_in_memory {
            state.payload = None;
        }
        Ok(Some(result))
    }

    /// Called only for unpooled geometry of a new GPU allocation. One shared transition
    /// schedules at most one reread; completion wakes pending block uploads.
    pub(crate) fn request_upload(
        &self,
        wake: Arc<AtomicU64>,
        failed: impl FnOnce(String) + Send + 'static,
    ) -> Result<bool, String> {
        if self.retain_in_memory {
            self.prepare_tangents()?;
        }
        let mut state = match self.upload.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => {
                wake.fetch_add(1, Ordering::Release);
                return Ok(false);
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("mesh payload lock poisoned".into());
            }
        };
        if let Some(reason) = &state.failure {
            return Err(reason.clone());
        }
        if let Some(payload) = &state.payload {
            return Ok(payload.tangents.is_some());
        }
        if state.rebuilding {
            if !state
                .waiters
                .iter()
                .any(|waiter| Arc::ptr_eq(waiter, &wake))
            {
                state.waiters.push(wake);
            }
            return Ok(false);
        }
        let source = self
            .source
            .clone()
            .ok_or("released mesh has no certified reread source")?;
        let terrain_only = state
            .terrain_only
            .ok_or("released mesh has no preparation mode")?;
        state.rebuilding = true;
        state.waiters.push(wake);
        drop(state);
        #[cfg(test)]
        self.upload.requests.fetch_add(1, Ordering::Relaxed);
        let mesh = self.clone();
        bevy::tasks::IoTaskPool::get()
            .spawn(async move {
                let rebuilt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let rebuilt = source.read_mesh()?;
                    rebuilt.prepare(terrain_only)?;
                    if rebuilt.prepared_tangents_usable() != mesh.prepared_tangents_usable() {
                        return Err(
                            "rehydrated tangent proof differs from initial preparation".into()
                        );
                    }
                    let mut state = rebuilt
                        .upload
                        .state
                        .lock()
                        .map_err(|_| "rehydrated payload lock poisoned")?;
                    state
                        .payload
                        .take()
                        .ok_or_else(|| "rehydrated payload is missing".to_string())
                }))
                .unwrap_or_else(|_| Err("mesh rehydration worker panicked".into()));
                // No local raw-byte/tangent owner survives this assignment and wakeup.
                let mut state = mesh
                    .upload
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.rebuilding = false;
                let waiters = std::mem::take(&mut state.waiters);
                match rebuilt {
                    Ok(payload) => state.payload = Some(payload),
                    Err(reason) => {
                        state.failure = Some(reason.clone());
                        drop(state);
                        failed(reason);
                        for waiter in waiters {
                            waiter.fetch_add(1, Ordering::Release);
                        }
                        return;
                    }
                }
                drop(state);
                for waiter in waiters {
                    waiter.fetch_add(1, Ordering::Release);
                }
            })
            .detach();
        Ok(false)
    }

    /// Logical unique upload-cell allocations for diagnostics, never an RSS claim.
    /// Rehydrate work in flight is reported separately; its temporary bytes are not measured here.
    #[cfg(test)]
    pub(super) fn resident_estimate(&self) -> Result<MeshResidentEstimate, String> {
        let state = self
            .upload
            .state
            .lock()
            .map_err(|_| "mesh payload lock poisoned")?;
        Ok(MeshResidentEstimate {
            metadata_bytes: std::mem::size_of::<Self>()
                + std::mem::size_of::<MeshUpload>()
                + self
                    .source
                    .as_ref()
                    .map_or(0, |source| source.metadata_bytes())
                + state.waiters.capacity() * std::mem::size_of::<Arc<AtomicU64>>()
                + state.failure.as_ref().map_or(0, String::capacity),
            scene_row_bytes: 0,
            raw_payload_bytes: state
                .payload
                .as_ref()
                .map_or(0, |payload| payload.bytes.len()),
            tangent_bytes: state
                .payload
                .as_ref()
                .and_then(|payload| payload.tangents.as_ref())
                .map_or(0, |tangents| {
                    tangents.words.len() * std::mem::size_of::<u32>()
                }),
            rehydrating_meshes: usize::from(state.rebuilding),
            planned_rehydrate_raw_bytes: if state.rebuilding { self.byte_len } else { 0 },
            planned_rehydrate_tangent_bytes: if state.rebuilding {
                self.vertex_count as usize * 4
            } else {
                0
            },
        })
    }

    #[cfg(test)]
    pub(super) fn upload_identity(&self) -> usize {
        Arc::as_ptr(&self.upload) as usize
    }

    #[cfg(test)]
    pub(crate) fn rebuild_request_count(&self) -> usize {
        self.upload.requests.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn poison_payload_for_test(&self) {
        let _guard = self.upload.state.lock().unwrap();
        panic!("injected tangent preparation panic");
    }

    #[cfg(test)]
    pub(crate) fn fail_tangents_for_test(&self) {
        let mut state = self.upload.state.lock().unwrap();
        let payload = state.payload.as_mut().unwrap();
        assert!(payload.tangents.is_none());
        payload.tangents = Some(prepare_generated_tangents(
            self.vertex_count as usize,
            false,
            None,
        ));
        state.terrain_only = Some(false);
        self.upload.proof.store(2, Ordering::Release);
    }

    fn generate_tangent_words(&self, view: &MclView<'_>) -> PreparedTangents {
        let count = self.vertex_count as usize;
        if self.flags & FLAG_TANGENTS != 0 {
            let bytes = view.section(MclSection::Tangents);
            let words: Vec<u32> = (0..count)
                .map(|vertex| {
                    u32::from_le_bytes([
                        bytes[vertex * 4],
                        bytes[vertex * 4 + 1],
                        bytes[vertex * 4 + 2],
                        bytes[vertex * 4 + 3],
                    ])
                })
                .collect();
            return PreparedTangents {
                words: words.into(),
                usable: true,
            };
        }
        let mut tangents = vec![[0.0; 4]; count];
        let succeeded = shared::cluster_tangents::generate_mcl_tangents(view, |vertex, tangent| {
            tangents[vertex] = tangent;
        });
        prepare_generated_tangents(
            count,
            succeeded,
            Some(&bevy::mesh::VertexAttributeValues::Float32x4(tangents)),
        )
    }

    /// The file size, for the upload budget and the byte metrics.
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }
}

/// A validated chunk instance block (`.mci`): the objects clusters own in one block of cells.
#[derive(Asset, TypePath, Clone)]
pub struct ClusterBlockAsset {
    pub builder_identity: [u8; 32],
    pub worldspace_id: u32,
    pub tier: u32,
    pub anchor_x: i32,
    pub anchor_y: i32,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    /// The mesh table: which `.mcl` payloads the instances name.
    pub meshes: Vec<MeshRef>,
    pub instances: Vec<InstanceRecord>,
}

impl ClusterBlockAsset {
    /// Validates `bytes` and copies the tables out (80 bytes per instance).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ClusterFormatError> {
        let view = read_mci(bytes)?;
        Ok(Self {
            builder_identity: view.builder_identity,
            worldspace_id: view.worldspace_id,
            tier: view.tier,
            anchor_x: view.anchor_x,
            anchor_y: view.anchor_y,
            bounds_min: view.bounds_min,
            bounds_max: view.bounds_max,
            meshes: (0..view.mesh_count as usize)
                .map(|index| view.mesh(index))
                .collect(),
            instances: (0..view.instance_count as usize)
                .map(|index| view.instance(index))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MCL: &[u8] = include_bytes!("../../../shared/tests/data/cluster/fixture.mcl");
    const MCI: &[u8] = include_bytes!("../../../shared/tests/data/cluster/fixture.mci");

    #[test]
    fn stock_source_preserves_alpha_and_colour_used_by_cutout_materials() {
        assert_eq!(
            source_colours(&[255, 128, 0, 0, 0, 64, 255, 127], 2),
            vec![
                [1.0, 128.0 / 255.0, 0.0, 0.0],
                [0.0, 64.0 / 255.0, 1.0, 127.0 / 255.0]
            ]
        );
        // Give the existing clean-room fixture an authored colour section so this assertion
        // fails if source_mesh drops RGBA, including alpha values on either side of a cutout.
        let mut bytes = MCL.to_vec();
        while !bytes
            .len()
            .is_multiple_of(shared::cluster::SECTION_ALIGNMENT)
        {
            bytes.push(0);
        }
        let offset = bytes.len();
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
        let rgba: Vec<u8> = (0..count)
            .flat_map(|vertex| [255, 128, 64, if vertex % 2 == 0 { 0 } else { 255 }])
            .collect();
        bytes.extend_from_slice(&rgba);
        let entry = shared::cluster::MCL_HEADER_BYTES + MclSection::Colours as usize * 16;
        bytes[entry..entry + 8].copy_from_slice(&(offset as u64).to_le_bytes());
        bytes[entry + 8..entry + 16].copy_from_slice(&(rgba.len() as u64).to_le_bytes());
        let flags = u32::from_le_bytes(bytes[76..80].try_into().unwrap())
            | shared::cluster::FLAG_VERTEX_COLOURS;
        bytes[76..80].copy_from_slice(&flags.to_le_bytes());
        let mesh = ClusterMeshAsset::from_bytes(bytes).unwrap();
        let source = mesh.source_mesh().unwrap();
        let Some(bevy::mesh::VertexAttributeValues::Float32x4(colours)) =
            source.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("source must expose RGBA");
        };
        assert_eq!(colours.len(), mesh.vertex_count as usize);
        assert_eq!(
            *colours,
            source_colours(
                &mesh.section(MclSection::Colours).unwrap(),
                mesh.vertex_count as usize
            )
        );
    }

    #[test]
    fn unused_terrain_slots_never_become_successful_tangent_proof_or_generate_later() {
        let mesh = ClusterMeshAsset::from_bytes(MCL.to_vec()).unwrap();
        assert_eq!(mesh.prepared_tangents_usable(), None);
        mesh.prepare_unused_tangent_slots().unwrap();
        let clone = mesh.clone();
        let words = mesh.tangent_words().unwrap();
        assert_eq!(words.len(), mesh.vertex_count as usize);
        assert_eq!(mesh.prepared_tangents_usable(), Some(false));
        let pointer = words.as_ptr();
        clone.prepare_tangents().unwrap();
        assert_eq!(clone.prepared_tangents_usable(), Some(false));
        assert_eq!(clone.tangent_words().unwrap().as_ptr(), pointer);
        assert!(
            words
                .iter()
                .all(|word| *word == pack_tangent_word([1.0, 0.0, 0.0, 1.0]))
        );
    }

    #[test]
    fn successful_generated_words_are_cached_and_shared_by_asset_clones() {
        let mesh = ClusterMeshAsset::from_bytes(generated_tangent_fixture_bytes()).unwrap();
        assert_eq!(
            mesh.flags & FLAG_TANGENTS,
            0,
            "fixture must exercise generation"
        );
        assert!(
            mesh.upload
                .state
                .lock()
                .unwrap()
                .payload
                .as_ref()
                .unwrap()
                .tangents
                .is_none()
        );
        assert_eq!(mesh.prepared_tangents_usable(), None);
        assert!(
            mesh.upload
                .state
                .lock()
                .unwrap()
                .payload
                .as_ref()
                .unwrap()
                .tangents
                .is_none()
        );
        let clone = mesh.clone();
        assert!(Arc::ptr_eq(&mesh.upload, &clone.upload));
        let words = mesh.tangent_words().unwrap();
        assert!(Arc::ptr_eq(
            &words,
            &mesh
                .upload
                .state
                .lock()
                .unwrap()
                .payload
                .as_ref()
                .unwrap()
                .tangents
                .as_ref()
                .unwrap()
                .words
        ));
        assert_eq!(words.len(), mesh.vertex_count as usize);
        assert!(mesh.tangents_usable());
        assert_eq!(mesh.prepared_tangents_usable(), Some(true));
        let pointer = words.as_ptr();
        clone.prepare_tangents().unwrap();
        assert!(clone.tangents_usable());
        let cloned_words = clone.tangent_words().unwrap();
        assert_eq!(cloned_words.as_ptr(), pointer);
        assert_eq!(cloned_words.as_ref(), words.as_ref());

        let mut source = mesh.source_mesh().unwrap();
        source.generate_tangents().unwrap();
        let Some(bevy::mesh::VertexAttributeValues::Float32x4(generated)) =
            source.attribute(Mesh::ATTRIBUTE_TANGENT)
        else {
            panic!("fixture must produce Bevy tangent words");
        };
        let expected: Vec<_> = generated.iter().copied().map(pack_tangent_word).collect();
        assert_eq!(words.as_ref(), expected);
    }

    #[test]
    fn successful_packing_keeps_handedness_and_zero_parent_only_slots() {
        let values = bevy::mesh::VertexAttributeValues::Float32x4(vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, -1.0],
            [0.0; 4],
        ]);
        let prepared = prepare_generated_tangents(3, true, Some(&values));
        assert!(prepared.usable);
        assert_eq!(prepared.words.as_ref(), [0x0000_7fff, 0x7fff_0000, 0]);
    }

    #[test]
    fn generation_and_attribute_failures_keep_diagnostic_words_but_are_unusable() {
        use bevy::mesh::VertexAttributeValues;
        let good = VertexAttributeValues::Float32x4(vec![[0.0, 1.0, 0.0, -1.0]; 2]);
        let wrong_type = VertexAttributeValues::Float32x3(vec![[1.0, 0.0, 0.0]; 2]);
        let short = VertexAttributeValues::Float32x4(vec![[1.0, 0.0, 0.0, 1.0]]);
        for (succeeded, attribute) in [
            (false, Some(&good)),
            (true, None),
            (true, Some(&wrong_type)),
            (true, Some(&short)),
        ] {
            let prepared = prepare_generated_tangents(2, succeeded, attribute);
            assert!(!prepared.usable);
            // Existing fallback is packed (+X,+1), unchanged for every vertex.
            assert_eq!(prepared.words.as_ref(), [0x0000_7fff; 2]);
        }
    }

    #[test]
    fn every_nonfinite_component_refuses_the_entire_generated_tangent_set() {
        for component in 0..4 {
            for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut tangents = vec![[0.0, 1.0, 0.0, -1.0]; 2];
                tangents[1][component] = invalid;
                let values = bevy::mesh::VertexAttributeValues::Float32x4(tangents);
                let prepared = prepare_generated_tangents(2, true, Some(&values));
                assert!(!prepared.usable);
                assert_eq!(prepared.words.as_ref(), [0x0000_7fff; 2]);
            }
        }
    }

    #[test]
    fn the_fixture_mesh_payload_loads() {
        let asset = ClusterMeshAsset::from_bytes(MCL.to_vec()).expect("fixture is valid");
        assert!(asset.cluster_count > 1, "a hierarchy has many clusters");
        assert!(asset.group_count > 0);
        assert!(asset.root_error > 0.0);
        assert_eq!(asset.byte_len(), MCL.len());
        let clusters = asset.section(MclSection::Clusters).unwrap();
        assert_eq!(clusters.len(), asset.cluster_count as usize * 48);
    }

    #[test]
    fn the_fixture_block_names_meshes_the_payload_matches() {
        let mesh = ClusterMeshAsset::from_bytes(MCL.to_vec()).unwrap();
        let block = ClusterBlockAsset::from_bytes(MCI).expect("fixture is valid");
        assert_eq!(block.instances.len(), 3, "three instances in two cells");
        assert!(
            block
                .meshes
                .iter()
                .any(|reference| reference.mesh_hash == mesh.mesh_hash
                    && reference.primitive_index == mesh.primitive_index)
        );
        assert_eq!(block.builder_identity, mesh.builder_identity);
    }

    #[test]
    fn a_truncated_or_corrupt_payload_fails_closed() {
        assert!(ClusterMeshAsset::from_bytes(MCL[..MCL.len() / 2].to_vec()).is_err());
        let mut bad_magic = MCL.to_vec();
        bad_magic[0] ^= 0xff;
        assert!(ClusterMeshAsset::from_bytes(bad_magic).is_err());
        assert!(ClusterBlockAsset::from_bytes(&MCI[..40]).is_err());
        let mut bad_version = MCI.to_vec();
        bad_version[4] = 9;
        assert!(ClusterBlockAsset::from_bytes(&bad_version).is_err());
    }
}

/// Preserve the real format fixture's geometry and hierarchy, but give every
/// distinct-vertex triangle noncollinear UVs for successful-generation tests.
/// The original format fixture has singular UV faces and remains a refusal control.
#[cfg(test)]
pub(crate) fn generated_tangent_fixture_bytes() -> Vec<u8> {
    let mut bytes = include_bytes!("../../../shared/tests/data/cluster/fixture.mcl").to_vec();
    let (start, length, count) = {
        let view = read_mcl_validated(&bytes).unwrap();
        let uvs = view.section(MclSection::Uvs);
        (
            uvs.as_ptr() as usize - bytes.as_ptr() as usize,
            uvs.len(),
            view.vertex_count as usize,
        )
    };
    // Points (i, i^2) over this 16-bit prime field cannot have three distinct
    // collinear indices. This bound is for the fixed test fixture, not game data.
    const PRIME: u32 = 65_521;
    assert!(count < PRIME as usize);
    assert_eq!(length, count * 4);
    for (index, uv) in bytes[start..start + length]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let index = index as u32;
        uv[..2].copy_from_slice(&(index as u16).to_le_bytes());
        uv[2..].copy_from_slice(&((index * index % PRIME) as u16).to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod generated_fixture_tests {
    use super::*;

    #[test]
    fn format_fixture_singular_uvs_are_not_successful_tangent_proof() {
        let mesh = ClusterMeshAsset::from_bytes(
            include_bytes!("../../../shared/tests/data/cluster/fixture.mcl").to_vec(),
        )
        .unwrap();
        mesh.prepare_tangents().unwrap();
        assert_eq!(mesh.prepared_tangents_usable(), Some(false));
        assert!(!mesh.tangents_usable());
    }

    #[test]
    fn successful_fixture_changes_only_uv_bytes_and_keeps_the_original_hierarchy() {
        let original = include_bytes!("../../../shared/tests/data/cluster/fixture.mcl");
        let changed = generated_tangent_fixture_bytes();
        assert_eq!(original.len(), changed.len());
        let before = read_mcl_validated(original).unwrap();
        let after = read_mcl_validated(&changed).unwrap();
        for section in [
            MclSection::Positions,
            MclSection::Normals,
            MclSection::Tangents,
            MclSection::Colours,
            MclSection::Clusters,
            MclSection::ClusterVertices,
            MclSection::ClusterTriangles,
            MclSection::Groups,
            MclSection::Bvh,
        ] {
            assert_eq!(before.section(section), after.section(section));
        }
        assert_eq!(before.builder_identity, after.builder_identity);
        assert_eq!(before.mesh_hash, after.mesh_hash);
        assert_eq!(before.uv_min, after.uv_min);
        assert_eq!(before.uv_scale, after.uv_scale);
        let mesh = ClusterMeshAsset::from_bytes(changed).unwrap();
        mesh.prepare_tangents().unwrap();
        assert_eq!(mesh.prepared_tangents_usable(), Some(true));
    }
}

/// Add an authored tangent section to the real hierarchy without changing its
/// source/parent clusters, vertices, groups or BVH records.
#[cfg(test)]
pub(crate) fn authored_tangent_fixture() -> ClusterMeshAsset {
    use shared::cluster::{MclContents, write_mcl};
    let source = ClusterMeshAsset::from_bytes(
        include_bytes!("../../../shared/tests/data/cluster/fixture.mcl").to_vec(),
    )
    .unwrap();
    let snapshot = source.snapshot().unwrap();
    let view = read_mcl_validated(&snapshot).unwrap();
    let count = source.vertex_count as usize;
    let positions: Vec<_> = (0..count).map(|i| view.position(i)).collect();
    let normals: Vec<_> = (0..count).map(|i| view.normal(i)).collect();
    let uvs: Vec<_> = (0..count).map(|i| view.uv(i)).collect();
    let tangents: Vec<_> = (0..count)
        .map(|i| {
            let word = pack_tangent_word(if i % 2 == 0 {
                [1.0, 0.0, 0.0, -1.0]
            } else {
                [0.0, 1.0, 0.0, 1.0]
            });
            [word as u16 as i16, (word >> 16) as u16 as i16]
        })
        .collect();
    let clusters: Vec<_> = (0..source.cluster_count as usize)
        .map(|i| view.cluster(i))
        .collect();
    let groups: Vec<_> = (0..source.group_count as usize)
        .map(|i| view.group(i))
        .collect();
    let bvh: Vec<_> = (0..source.bvh_node_count as usize)
        .map(|i| view.bvh_node(i))
        .collect();
    let vertices: Vec<_> = (0..view.section(MclSection::ClusterVertices).len() / 4)
        .map(|i| view.cluster_vertex(i))
        .collect();
    let triangles: Vec<[u8; 3]> = view
        .section(MclSection::ClusterTriangles)
        .as_chunks::<3>()
        .0
        .iter()
        .map(|v| [v[0], v[1], v[2]])
        .collect();
    let bytes = write_mcl(&MclContents {
        builder_identity: view.builder_identity,
        mesh_hash: view.mesh_hash,
        primitive_index: view.primitive_index,
        root_error: view.root_error,
        bounds: view.bounds,
        uv_min: view.uv_min,
        uv_scale: view.uv_scale,
        positions: &positions,
        normals: &normals,
        uvs: &uvs,
        tangents: Some(&tangents),
        colours: None,
        clusters: &clusters,
        cluster_vertices: &vertices,
        cluster_triangles: &triangles,
        groups: &groups,
        bvh: &bvh,
    })
    .unwrap();
    ClusterMeshAsset::from_bytes(bytes).unwrap()
}

#[cfg(test)]
mod source_tangent_regressions {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    #[test]
    fn source_mesh_keeps_authored_tangents_without_a_normal_map() {
        let asset = authored_tangent_fixture();
        let source = asset.source_mesh().unwrap();
        let Some(VertexAttributeValues::Float32x4(tangents)) =
            source.attribute(Mesh::ATTRIBUTE_TANGENT)
        else {
            panic!("authored TANGENT semantic was dropped");
        };
        assert_eq!(tangents.len(), asset.vertex_count as usize);
        assert!(
            tangents
                .iter()
                .enumerate()
                .all(|(i, t)| t[3] == if i % 2 == 0 { -1.0 } else { 1.0 })
        );
        let mut mapped = asset.source_mesh().unwrap();
        if !mapped.contains_attribute(Mesh::ATTRIBUTE_TANGENT) {
            mapped.generate_tangents().unwrap();
        }
        assert_eq!(
            mapped.attribute(Mesh::ATTRIBUTE_TANGENT),
            source.attribute(Mesh::ATTRIBUTE_TANGENT)
        );
        asset.prepare_tangents().unwrap();
        let expected: Vec<_> = asset
            .section(MclSection::Tangents)
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            .collect();
        assert_eq!(asset.tangent_words().unwrap().as_ref(), expected);
    }

    #[test]
    fn source_tangent_direction_masks_the_handedness_bit_like_the_cluster_shader() {
        let asset = authored_tangent_fixture();
        let source = asset.source_mesh().unwrap();
        let Some(VertexAttributeValues::Float32x4(tangents)) =
            source.attribute(Mesh::ATTRIBUTE_TANGENT)
        else {
            panic!("missing authored tangents")
        };
        for (tangent, bytes) in tangents.iter().zip(
            asset
                .section(MclSection::Tangents)
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter(),
        ) {
            let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let direction =
                decode_oct_snorm16([word as u16 as i16, ((word >> 16) as u16 & !1) as i16]);
            assert_eq!(&tangent[..3], &direction);
        }
    }
}

#[cfg(test)]
#[path = "assets_upload_tests.rs"]
mod upload_tests;
