use std::collections::{BTreeMap, BTreeSet};

use gltf::json::accessor::ComponentType;
use gltf::json::accessor::GenericComponentType;
use gltf::json::buffer::View;
use gltf::json::extensions::material::PbrDiffuseFactor;
use gltf::json::extensions::material::PbrSpecularFactor;
use gltf::json::extensions::material::PbrSpecularGlossiness;
use gltf::json::material::EmissiveFactor;
use gltf::json::material::NormalTexture;
use gltf::json::material::PbrBaseColorFactor;
use gltf::json::material::PbrMetallicRoughness;
use gltf::json::material::StrengthFactor;
use gltf::json::mesh::Primitive;
use gltf::json::scene::UnitQuaternion;
use gltf::json::texture::Info;
use gltf::json::validation::{Checked, USize64};
use gltf::{json::*, Semantic};
use project_wormhole_ba2::dev::ensure_texture_parent;
use project_wormhole_ba2::dev::normalize_esm_path;
use project_wormhole_esm::structs::geometry::Quaternion;
use project_wormhole_shared::glam;
use project_wormhole_shared::glam::{Mat4, U8Vec4, Vec3, Vec4};
use zerocopy::IntoBytes;

use crate::model::all::*;

use super::dev::*;

#[derive(Debug)]
pub struct NifFile {
    pub header: NifHeader,
    pub blocks: Vec<NifBlock>,
}

/// The skin-instance blocks a shape can point at through `BSTriShape::skin`.
///
/// Skyrim SE actor meshes use `BSDismemberSkinInstance`, older content uses the
/// plain `NiSkinInstance`, and Fallout 4 uses `BSSkinInstance`. The three carry
/// different payloads, so the caller matches on the variant it actually needs.
#[derive(Debug, Clone, Copy)]
pub enum SkinInstanceRef<'a> {
    Bs(&'a BSSkinInstance),
    Ni(&'a NiSkinInstance),
    Dismember(&'a BsDismemberSkinInstance),
}

impl NifFile {
    pub fn open(path: &str) -> Result<Self, std::io::Error> {
        // Open file for read
        let mut file = std::fs::File::open(path)?;

        // Init buffer
        let mut buf = Vec::new();

        // Read file
        file.read_to_end(&mut buf)?;

        // Parse nif file
        let (_, nif) = NifFile::parse(&buf).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid or unsupported NIF",
            )
        })?;

        Ok(nif)
    }

    pub fn get_nodes(&self) -> Vec<&NiNode> {
        self.blocks
            .iter()
            .filter_map(|block| block.as_node().ok())
            .collect()
    }

    pub fn get_skins(&self) -> Vec<&BSSkinInstance> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                NifBlock::BSSkinInstance(skin) => Some(skin),
                _ => None,
            })
            .collect()
    }

    pub fn skin_instance_at(&self, block: u32) -> Option<SkinInstanceRef<'_>> {
        match self.blocks.get(block as usize)? {
            NifBlock::BSSkinInstance(skin) => Some(SkinInstanceRef::Bs(skin)),
            NifBlock::NiSkinInstance(skin) => Some(SkinInstanceRef::Ni(skin)),
            NifBlock::BsDismemberSkinInstance(skin) => Some(SkinInstanceRef::Dismember(skin)),
            _ => None,
        }
    }

    pub fn get_shapes(&self) -> Vec<&BSTriShape> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                NifBlock::BSTriShape(shape) => Some(shape),
                _ => None,
            })
            .collect()
    }

    pub fn get_dynamic_shapes(&self) -> Vec<&BSDynamicTriShape> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                NifBlock::BSDynamicTriShape(shape) => Some(shape),
                _ => None,
            })
            .collect()
    }

    pub fn get_seg_shapes(&self) -> Vec<&BSSubIndexTriShape> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                NifBlock::BSSubIndexTriShape(shape) => Some(shape),
                _ => None,
            })
            .collect()
    }

    pub fn get_lod_shapes(&self) -> Vec<&BSLODTriShape> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                NifBlock::BSLODTriShape(shape) => Some(shape),
                _ => None,
            })
            .collect()
    }

    pub fn get_legacy_shapes(&self) -> Vec<(&NiTriShape, &NiTriShapeData)> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                NifBlock::NiTriShape(shape) => {
                    self.blocks
                        .get(shape.data as usize)
                        .and_then(|data| match data {
                            NifBlock::NiTriShapeData(data) => Some((shape, data)),
                            _ => None,
                        })
                }
                _ => None,
            })
            .collect()
    }

    pub fn get_materials(&self) -> Vec<&BSShaderTextureSet> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                NifBlock::BSShaderTextureSet(set) => Some(set),
                _ => None,
            })
            .collect()
    }

    /// Material id per exported mesh, in block order.
    ///
    /// Only `BSSubIndexTriShape` contributes an entry. `BSTriShape` materials are
    /// not tracked by this parsed-block view — the raw-block view
    /// (`RawNifFile::get_mesh_materials_ids`) does track them — so a file whose
    /// geometry is all `BSTriShape` yields an empty list. A caller that emits one
    /// mesh per shape must not index the result without accounting for the
    /// missing entries; `to_glb` binds `material_indices[mesh]` for skeletal
    /// meshes, so a short list panics there rather than falling back to material 0.
    pub fn get_mesh_materials_ids(&self) -> Vec<u8> {
        let mut positions = Vec::new();
        let mut mat_index = 1;

        for (_index, block) in self.blocks.iter().enumerate() {
            match block {
                NifBlock::BSSubIndexTriShape(_shape) => {
                    positions.push(mat_index - 1);
                }

                NifBlock::BSShaderTextureSet(_textures) => {
                    mat_index += 1;
                }
                _ => {}
            }
        }
        positions
    }

    pub fn get_node_ref(&mut self, node_name: &str) -> Result<&mut NiNode, String> {
        if let Some(pos) = self.get_node_pos(node_name) {
            match &mut self.blocks[pos] {
                NifBlock::NiNode(node) => return Ok(node),
                NifBlock::BSMultiBoundNode(block) => return Ok(&mut block.node),
                _ => {}
            }
        }
        Err("Node not found".to_string())
    }

    pub fn get_node_pos(&self, node_name: &str) -> Option<usize> {
        for (index, block) in self.blocks.iter().enumerate() {
            let node = match block {
                NifBlock::NiNode(node) => node,
                NifBlock::BSMultiBoundNode(block) => &block.node,
                _ => continue,
            };
            if let Ok(name) = self.header.get_string(node.name() as usize) {
                if name == node_name {
                    return Some(index);
                }
            }
        }
        None
    }

    pub fn has_skeleton(&self) -> bool {
        self.declares_skin()
    }

    pub fn declares_skin(&self) -> bool {
        self.blocks.iter().any(|block| {
            matches!(
                block,
                NifBlock::BSSkinInstance(_)
                    | NifBlock::NiSkinInstance(_)
                    | NifBlock::BsDismemberSkinInstance(_)
            )
        })
    }

    /// The unique node names every legacy skin instance binds, in joint order.
    pub fn legacy_skin_joint_names(&self) -> BTreeSet<String> {
        legacy_skin_joints(self)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }
}

impl Parse<&[u8]> for NifFile {
    /// Parses a NIF header and its size-delimited blocks using the declared Bethesda version.
    fn parse(i: &[u8]) -> nom::IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        // Parse header first
        let (i, header) = NifHeader::parse(i)?;

        // Init block vector
        let mut blocks = Vec::new();

        // Set data to the rest of the buffer
        let mut data = i;

        // Parse blocks
        for index in 0..header.block_count as usize {
            let (i, raw) =
                take::<u32, &[u8], nom::error::Error<&[u8]>>(header.block_size_index[index])(data)?;
            data = i;
            let (_, block) = NifBlock::parse_with_version(
                raw,
                header.get_block_type(index).unwrap().to_string(),
                header.bethesda_version,
            )?;
            blocks.push(block);
        }

        // Create nif file structure
        let nif = Self { header, blocks };

        if data.len() != 0 {
            warn!("Leftover data from NifFile::parse() : {:?}", data.len());
        }

        // Return nif file, and leftover data (should be zero leftover)
        Ok((i, nif))
    }
}

pub fn nif_to_model(nif: &NifFile, skeleton: Option<&NifFile>) -> Result<Model, String> {
    // Init empty model
    let mut model = Model::default();

    // Check if nif is a skeletal mesh
    if nif.has_skeleton() {
        // Init skeletal mesh
        let mut mesh = SkeletalMesh::default();

        // A provided skeleton supplies the bone tree. Without one the mesh is
        // a self-contained rig: its own joints and their ancestors are the only
        // bones it has. The resolver decides which case applies; an unresolved
        // skin is rejected before it reaches the exporter.
        match skeleton {
            Some(skeleton) => {
                // Get nodes from skeleton
                let skel_nodes = skeleton.get_nodes();
                // The first block with a node name owns that name, matching the order this
                // loop keeps bones in. Children pointing at a later duplicate are dropped so
                // the duplicate cannot stay attached to a second parent.
                let mut winning_block = BTreeMap::new();
                for (block, node) in skeleton.blocks.iter().enumerate() {
                    let Ok(node) = node.as_node() else {
                        continue;
                    };
                    let Ok(name) = skeleton.header.get_string(node.name() as usize) else {
                        continue;
                    };
                    if let Ok(block) = u32::try_from(block) {
                        winning_block.entry(name.to_string()).or_insert(block);
                    }
                }

                // Loop through nodes and add them to the bone tree
                let mut duplicate_names = BTreeSet::new();
                let mut claimed_children = BTreeSet::new();
                for skel_node in skel_nodes {
                    // Get node name
                    let node_name = skeleton
                        .header
                        .get_string(skel_node.name() as usize)
                        .map_err(|error| format!("skeleton node has an invalid name: {error}"))?;

                    // Get children names from string indices. Only the block
                    // variants `get_nodes` walks may become bones, so a child of
                    // any other type (and an unresolved block reference) is
                    // reported and dropped: naming it here would add a child
                    // without a matching bone entry, which the exporter cannot
                    // wire.
                    let children_names = skel_node
                        .children
                        .iter()
                    .filter_map(|index| {
                        let Some(child_block) = skeleton.blocks.get(*index as usize) else {
                            warn!(
                                "skeleton node {node_name:?} references child block {index}, which is out of range; skipping it"
                            );
                            return None;
                        };
                        let child_node = match child_block.as_node() {
                            Ok(child) => child,
                            Err(_) => {
                                warn!(
                                    "skeleton node {node_name:?} references child block {index} of type {child_block:?}, which is not a skeleton node; skipping it"
                                );
                                return None;
                            }
                        };
                        match skeleton.header.get_string(child_node.name() as usize) {
                            Ok(name) if winning_block.get(name) == Some(index) => {
                                Some(name.to_string())
                            }
                            Ok(name) => {
                                warn!(
                                    "skeleton node {node_name:?} references child block {index} ({name:?}), which repeats a node name; skipping the duplicate"
                                );
                                None
                            }
                            Err(error) => {
                                warn!(
                                    "skeleton node {node_name:?} child block {index} has an invalid name: {error}; skipping it"
                                );
                                None
                            }
                        }
                    })
                    .collect::<Vec<String>>();

                    // Add bone to bone tree. Retail skeletons repeat a few helper
                    // leaves no skin binds, so the first occurrence wins and the
                    // later copy is dropped whole (its children are duplicates
                    // too). A skin that does bind such a name is rejected below.
                    if mesh.bones.has_bone(node_name) {
                        warn!(
                            "skeleton node {node_name:?} appears more than once; keeping the first occurrence"
                        );
                        duplicate_names.insert(node_name.to_string());
                        continue;
                    }
                    // A shared child block also needs one parent. Dropped duplicate bones
                    // must not claim children that a retained parent can still attach.
                    let children = children_names
                        .into_iter()
                        .filter(|child| {
                            if claimed_children.insert(child.clone()) {
                                true
                            } else {
                                warn!(
                                    "skeleton child {child:?} already has a parent; skipping the reference from {node_name:?}"
                                );
                                false
                            }
                        })
                        .collect();

                    // Create bone data
                    let bone_data = BoneData {
                        transform: BoneTransform::new(
                            Some(skel_node.translation().0),
                            Some(skel_node.rotation().0),
                            Some(skel_node.scale().0),
                        ),
                        children,
                    };
                    mesh.bones.add_bone(node_name, bone_data)?;
                }

                // Either copy could be the bone the skin was authored against,
                // and merging them would invent a joint the file does not hold.
                let bound_duplicates = legacy_skin_joints(nif)
                    .into_iter()
                    .map(|(_, name)| name)
                    .filter(|name| duplicate_names.contains(name))
                    .collect::<BTreeSet<_>>();
                if !bound_duplicates.is_empty() {
                    return Err(format!(
                        "skin joints {bound_duplicates:?} name more than one skeleton node, so the bone they bind is ambiguous"
                    ));
                }

                bind_mesh_local_legacy_joints(nif, &mut mesh.bones);
            }
            None => bind_self_contained_joints(nif, &mut mesh.bones),
        }

        let (meshes, material_indices) = if nif.get_skins().is_empty() {
            legacy_skinned_meshes(nif, &mesh.bones)?
        } else {
            (bsskin_meshes(nif, &mesh), nif.get_mesh_materials_ids())
        };

        model.skeletal_meshes = meshes;

        let material = nif.get_materials();

        for mat in material {
            let diffuse = mat.diffuse.clone();
            let normal = mat.normal.clone();
            let glow = mat.glow.clone();
            let specular = mat.specular.clone();

            model.materials.push(super::model::all::Material {
                diffuse,
                normal,
                glow,
                specular,
            });
        }

        model.material_indices = material_indices;
    }
    // Proceed with static mesh conversion
    else {
        populate_static_scene(nif, &mut model)?;

        let material = nif.get_materials();
        let mat_indices = nif.get_mesh_materials_ids();

        for mat in material {
            let diffuse = mat.diffuse.clone();
            let normal = mat.normal.clone();
            let glow = mat.glow.clone();
            let specular = mat.specular.clone();

            model.materials.push(super::model::all::Material {
                diffuse,
                normal,
                glow,
                specular,
            });
        }
    }

    fill_model_space_normals(nif, &mut model);

    Ok(model)
}

/// Builds the Fallout 4 `BSSkinInstance` mesh list.
fn bsskin_meshes(nif: &NifFile, mesh: &SkeletalMesh) -> Vec<SkeletalMesh> {
    let mut mesh = mesh.clone();

    // Init empty mesh vector
    let mut meshes: Vec<SkeletalMesh> = Vec::new();

    for (index, block) in nif.blocks.iter().enumerate() {
        let NifBlock::BSTriShape(shape) = block else {
            continue;
        };
        let name = Some(
            nif.header
                .get_string(shape.av.object.name as usize)
                .unwrap()
                .to_string(),
        );
        mesh.mesh = Some(tri_shape_to_mesh(shape, name));
        mesh.source_block = Some(index as u32);
        mesh.weights = shape.get_vertex_weights();
        mesh.joints = shape.get_vertex_joints();
        meshes.push(mesh.clone());
    }

    for (index, block) in nif.blocks.iter().enumerate() {
        let NifBlock::BSDynamicTriShape(dynamic) = block else {
            continue;
        };
        let shape = &dynamic.bs_tri_shape;
        let name = Some(
            nif.header
                .get_string(shape.av.object.name as usize)
                .unwrap()
                .to_string(),
        );
        mesh.mesh = Some(tri_shape_to_mesh(shape, name));
        mesh.source_block = Some(index as u32);
        mesh.weights = shape.get_vertex_weights();
        mesh.joints = shape.get_vertex_joints();
        meshes.push(mesh.clone());
    }

    for (index, block) in nif.blocks.iter().enumerate() {
        let NifBlock::BSSubIndexTriShape(seg) = block else {
            continue;
        };
        let shape = &seg.bs_tri_shape;
        let name = Some(
            nif.header
                .get_string(shape.av.object.name as usize)
                .unwrap()
                .to_string(),
        );
        mesh.mesh = Some(tri_shape_to_mesh(shape, name));
        mesh.source_block = Some(index as u32);
        mesh.weights = shape.get_vertex_weights();
        mesh.joints = shape.get_vertex_joints();
        meshes.push(mesh.clone());
    }

    for (skin_index, skin) in nif.get_skins().iter().enumerate() {
        let mut inverse_bind_matrices = Vec::new();

        // Get the inverse position matrices
        match &nif.blocks[skin.data as usize] {
            NifBlock::BSSkinBoneData(bone_data) => {
                for bone_transform in &bone_data.bone_list {
                    inverse_bind_matrices.push(bone_transform.into_matrix4());
                }
            }
            _ => {
                panic!("Inverse position block is not a BSSkinBoneData")
            }
        }

        let mut joint_names = Vec::new();
        warn!("{:?}", skin.bones.len());

        for joint_index in &skin.bones {
            match &nif.blocks[*joint_index as usize] {
                NifBlock::NiNode(node) => {
                    joint_names.push(
                        nif.header
                            .get_string(node.name() as usize)
                            .unwrap()
                            .to_string(),
                    );
                }
                _ => {
                    panic!("Skin bone is not a NiNode")
                }
            }
        }

        debug!("Joint names: {:?}", joint_names);

        meshes[skin_index].skin = joint_names;
        meshes[skin_index].inverse_bind_matrices = inverse_bind_matrices;
    }

    meshes
}

fn legacy_skinned_meshes(
    nif: &NifFile,
    bones: &BoneTree,
) -> Result<(Vec<SkeletalMesh>, Vec<u8>), String> {
    let mut meshes = Vec::new();
    let mut material_indices = Vec::new();

    for (index, block) in nif.blocks.iter().enumerate() {
        let (shape, block_type) = match block {
            NifBlock::BSTriShape(shape) => (shape, "BSTriShape"),
            NifBlock::BSDynamicTriShape(dynamic) => (&dynamic.bs_tri_shape, "BSDynamicTriShape"),
            _ => continue,
        };
        let shape_index = index as u32;
        let name = nif
            .header
            .get_string(shape.av.object.name as usize)
            .ok()
            .map(str::to_owned);

        let instance_block = shape.skin;
        let Some(instance) = nif.skin_instance_at(instance_block) else {
            // A skinned file may still contain unskinned shapes (effects,
            // helpers, rigid trap and ruin parts). A SkeletalMesh cannot carry
            // an unskinned primitive; preserving them is a later #273 stage.
            warn!(
                "shape block {shape_index} ({block_type}, {name:?}) references no skin instance (block {instance_block}); leaving it out of the skeletal mesh set"
            );
            continue;
        };

        let instance = match instance {
            SkinInstanceRef::Ni(instance) => instance,
            SkinInstanceRef::Dismember(instance) => &instance.skin_instance,
            SkinInstanceRef::Bs(_) => {
                warn!(
                    "shape block {shape_index} ({block_type}, {name:?}) references BSSkinInstance block {instance_block}; the SSE BSSkin path handles those shapes"
                );
                continue;
            }
        };

        meshes.push(legacy_skinned_mesh(
            nif,
            shape_index,
            block_type,
            shape,
            name,
            instance,
            instance_block,
            bones,
        )?);
        material_indices.push(shape_material_index(
            nif,
            shape_index,
            block_type,
            shape.shader_property,
        ));
    }

    Ok((meshes, material_indices))
}

/// Material index for one emitted shape, counted in `NifFile::get_materials`
/// order (`BSShaderTextureSet` block order). Falls back to material 0 when the
/// shape has no lighting shader or its texture set is not in the file.
fn shape_material_index(
    nif: &NifFile,
    shape_index: u32,
    block_type: &str,
    shader_property: u32,
) -> u8 {
    let texture_set = match nif.blocks.get(shader_property as usize) {
        Some(NifBlock::BSLightingShaderProperty(property)) => property.texture_set,
        _ => {
            debug!(
                "shape block {shape_index} ({block_type}) has no BSLightingShaderProperty at block {shader_property}; binding material 0"
            );
            return 0;
        }
    };

    let mut material = 0u8;
    for (index, block) in nif.blocks.iter().enumerate() {
        let NifBlock::BSShaderTextureSet(_) = block else {
            continue;
        };
        if index as u32 == texture_set {
            return material;
        }
        match material.checked_add(1) {
            Some(next) => material = next,
            None => break,
        }
    }

    debug!(
        "shape block {shape_index} ({block_type}) references texture set block {texture_set}, which is not in the material list; binding material 0"
    );
    0
}

/// Below half coverage the skeleton is more likely the wrong one than the mesh carrying the extras.
const MIN_SKELETON_COVERAGE: f32 = 0.5;

/// The `(block, name)` pair for every node a legacy skin instance binds, in
/// joint order.
fn legacy_skin_joint_nodes(
    nif: &NifFile,
    instance: &NiSkinInstance,
) -> Result<Vec<(u32, String)>, String> {
    let mut joint_nodes = Vec::with_capacity(instance.bones.len());
    for joint in &instance.bones {
        let Some(node) = nif
            .blocks
            .get(*joint as usize)
            .and_then(|block| block.as_node().ok())
        else {
            return Err(format!("bone reference {joint} is not a NiNode"));
        };
        let name = nif
            .header
            .get_string(node.name() as usize)
            .map_err(|error| format!("bone block {joint} has an invalid name: {error}"))?;
        joint_nodes.push((*joint, name.to_string()));
    }
    Ok(joint_nodes)
}

fn nif_node(nif: &NifFile, block: u32) -> Option<&NiNode> {
    nif.blocks.get(block as usize)?.as_node().ok()
}

fn nif_node_name(nif: &NifFile, block: u32) -> Option<&str> {
    nif.header
        .get_string(nif_node(nif, block)?.name() as usize)
        .ok()
}

/// Every legacy skin joint across the mesh's instances, deduplicated by name in
/// joint order.
fn legacy_skin_joints(nif: &NifFile) -> Vec<(u32, String)> {
    let mut seen = BTreeSet::new();
    let mut joints = Vec::new();
    for (index, _) in nif.blocks.iter().enumerate() {
        let Some(instance) = nif.skin_instance_at(index as u32) else {
            continue;
        };
        let instance = match instance {
            SkinInstanceRef::Ni(instance) => instance,
            SkinInstanceRef::Dismember(instance) => &instance.skin_instance,
            SkinInstanceRef::Bs(_) => continue,
        };
        let Ok(nodes) = legacy_skin_joint_nodes(nif, instance) else {
            continue;
        };
        for (block, name) in nodes {
            if seen.insert(name.clone()) {
                joints.push((block, name));
            }
        }
    }
    joints
}

/// Adds the legacy skin joints the skeleton does not carry, together with the
/// ancestors they need, from the mesh's own node hierarchy.
///
/// A legacy NIF copies its whole bone hierarchy, so binding every missing joint
/// would also make a mesh bound to the wrong skeleton look resolved. Only a
/// skeleton that already covers most joints is treated as the right one.
fn bind_mesh_local_legacy_joints(nif: &NifFile, bones: &mut BoneTree) {
    let joints = legacy_skin_joints(nif);
    let total = joints.len();
    let missing = joints
        .into_iter()
        .filter(|(_, name)| !bones.has_bone(name))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }
    let covered = total - missing.len();
    if (covered as f32 / total as f32) < MIN_SKELETON_COVERAGE {
        return;
    }
    let bound = insert_mesh_local_joint_chains(nif, bones, missing);
    if !bound.is_empty() {
        warn!(
            "legacy skin joints {bound:?} are missing from the skeleton and were bound from the mesh's own node hierarchy"
        );
    }
}

/// A self-contained rig has no skeleton to cover its joints, so its own
/// hierarchy is the bone tree and every joint goes in without a coverage gate.
fn bind_self_contained_joints(nif: &NifFile, bones: &mut BoneTree) {
    let missing = legacy_skin_joints(nif)
        .into_iter()
        .filter(|(_, name)| !bones.has_bone(name))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }
    let bound = insert_mesh_local_joint_chains(nif, bones, missing);
    if !bound.is_empty() {
        warn!(
            "legacy skin joints {bound:?} are bound from the mesh's own node hierarchy because no skeleton covers the rig"
        );
    }
}

fn insert_mesh_local_joint_chains(
    nif: &NifFile,
    bones: &mut BoneTree,
    missing: Vec<(u32, String)>,
) -> Vec<String> {
    let mut parents = BTreeMap::new();
    for (index, block) in nif.blocks.iter().enumerate() {
        let Ok(node) = block.as_node() else {
            continue;
        };
        let children = &node.children;
        for child in children {
            parents.entry(*child).or_insert(index as u32);
        }
    }

    let mut bound = Vec::new();
    for (block, name) in missing {
        let mut chain = vec![(block, name.clone())];
        let mut current = block;
        let mut parent_bone = None;
        let mut visited = BTreeSet::from([block]);
        while let Some(&parent) = parents.get(&current) {
            // Block 0 is the scene root: the placing reference replaces its
            // transform, so it never becomes a bone.
            if parent == 0 {
                break;
            }
            let Some(parent_name) = nif_node_name(nif, parent) else {
                break;
            };
            if bones.has_bone(parent_name) {
                parent_bone = Some(parent_name.to_string());
                break;
            }
            if !visited.insert(parent) {
                break;
            }
            chain.push((parent, parent_name.to_string()));
            current = parent;
        }

        // `chain` is joint-first: each entry sits directly below the next one,
        // so a bone's children point one index down, never at the node above.
        let mut unique = BTreeSet::new();
        let insertable = chain
            .iter()
            .all(|(_, chain_name)| !bones.has_bone(chain_name) && unique.insert(chain_name));
        if !insertable {
            continue;
        }
        let chain_nodes = chain
            .iter()
            .map(|(chain_block, _)| nif_node(nif, *chain_block))
            .collect::<Option<Vec<_>>>();
        let Some(chain_nodes) = chain_nodes else {
            continue;
        };

        let mut landed = true;
        for (index, ((_, chain_name), node)) in chain.iter().zip(&chain_nodes).enumerate() {
            let children = index
                .checked_sub(1)
                .map(|below| chain[below].1.clone())
                .into_iter()
                .collect();
            let bone_data = BoneData {
                transform: BoneTransform::new(
                    Some(node.translation().0),
                    Some(node.rotation().0),
                    Some(node.scale().0),
                ),
                children,
            };
            if bones.add_bone(chain_name, bone_data).is_err() {
                landed = false;
                break;
            }
        }
        if !landed {
            continue;
        }

        if let Some(parent_bone) = parent_bone {
            if let Some(parent) = bones.bones.get_mut(&parent_bone) {
                parent.children.insert(chain[chain.len() - 1].1.clone());
            }
        }
        bound.push(name);
    }

    bound
}

fn legacy_skinned_mesh(
    nif: &NifFile,
    shape_index: u32,
    block_type: &str,
    shape: &BSTriShape,
    name: Option<String>,
    instance: &NiSkinInstance,
    instance_block: u32,
    bones: &BoneTree,
) -> Result<SkeletalMesh, String> {
    let skin_data = match nif.blocks.get(instance.data as usize) {
        Some(NifBlock::NiSkinData(data)) => data,
        _ => {
            return Err(format!(
                "shape block {shape_index} ({block_type}): skin instance block {instance_block} references block {} which is not NiSkinData",
                instance.data
            ))
        }
    };

    if skin_data.bones.len() != instance.bones.len() {
        return Err(format!(
            "shape block {shape_index} ({block_type}): skin instance block {instance_block} lists {} bones but NiSkinData block {} has {} inverse bind transforms",
            instance.bones.len(),
            instance.data,
            skin_data.bones.len()
        ));
    }

    let joint_names = legacy_skin_joint_nodes(nif, instance)
        .map_err(|error| {
            format!(
                "shape block {shape_index} ({block_type}): skin instance block {instance_block}: {error}"
            )
        })?
        .into_iter()
        .map(|(_, name)| name)
        .collect::<Vec<String>>();

    let inverse_bind_matrices = skin_data
        .bones
        .iter()
        .map(|bone| bone.skin_transform.into_matrix4())
        .collect();

    let partition = match nif.blocks.get(instance.skin_partition as usize) {
        Some(NifBlock::NiSkinPartition(partition)) => partition,
        _ => {
            return Err(format!(
                "shape block {shape_index} ({block_type}): skin instance block {instance_block} references block {} which is not NiSkinPartition",
                instance.skin_partition
            ))
        }
    };

    let mesh = legacy_shape_mesh(shape, partition, name.clone());
    let position_count = mesh.positions.len();
    if position_count == 0 {
        return Err(format!(
            "shape block {shape_index} ({block_type}) is skinned but has no vertex positions in the shape, its dynamic vertex array or NiSkinPartition block {}",
            instance.skin_partition
        ));
    }

    let (influences, indexing) = vertex_influences(
        shape_index,
        block_type,
        shape,
        instance.skin_partition,
        partition,
        position_count,
    )?;
    if influences.len() != position_count {
        return Err(format!(
            "shape block {shape_index} ({block_type}) has {} weighted vertices but {position_count} positions",
            influences.len()
        ));
    }
    if indexing == InfluenceIndices::PartitionPalette && partition.palette_overflow {
        return Err(format!(
            "shape block {shape_index} ({block_type}): NiSkinPartition block {} merges more than 255 bones, so its bone indices cannot address the merged palette",
            instance.skin_partition
        ));
    }

    let palette = match indexing {
        InfluenceIndices::InstanceBones => None,
        InfluenceIndices::PartitionPalette => Some(partition.bone_palette.as_slice()),
    };
    // Retail SSE shared vertex buffers carry padding vertices that no partition
    // maps and no triangle draws: all four weights 0.0 and bone indices
    // (0, 0, 0, 0) (118 vanilla NIFs, 2,724 vertices, e.g. the 1st-person
    // daedric cuirass). They never reach the screen, so they keep their place
    // in the buffer (morphs and FaceGen .tri files address vertices by index)
    // and take the binding their stored first index already names, at full
    // weight. Only that exact pattern is bound: a zero-weight vertex that a
    // triangle draws, that a partition's vertex map lists, or that names other
    // bones is still refused. Padding exists only in the vertex buffer, so
    // weights read from partition rows never qualify, and when partitions
    // disagreed on carrying a map the merged map is gone and nothing does.
    let padding_allowed =
        indexing == InfluenceIndices::InstanceBones && !partition.vertex_map_discarded;
    let mut in_use = vec![false; position_count];
    let drawn = mesh
        .triangles
        .iter()
        .chain(&partition.triangles)
        .flat_map(|triangle| triangle.to_array());
    let mapped = partition.vertex_map.iter().flatten().copied();
    for vertex in drawn.chain(mapped) {
        if let Some(slot) = in_use.get_mut(usize::from(vertex)) {
            *slot = true;
        }
    }
    let mut joints = Vec::with_capacity(position_count);
    let mut weights = Vec::with_capacity(position_count);
    for (vertex_index, (raw_joints, raw_weights)) in influences.into_iter().enumerate() {
        let raw_weights = if padding_allowed
            && !in_use[vertex_index]
            && raw_joints == [0; 4]
            && raw_weights == [0.0; 4]
        {
            [1.0, 0.0, 0.0, 0.0]
        } else {
            raw_weights
        };
        let (vertex_joints, vertex_weights) = normalize_vertex_influences(
            raw_joints,
            raw_weights,
            palette,
            instance.bones.len(),
            shape_index,
            vertex_index,
        )?;
        joints.push(vertex_joints);
        weights.push(vertex_weights);
    }

    Ok(SkeletalMesh {
        name,
        source_block: Some(shape_index),
        mesh: Some(mesh),
        weights,
        joints,
        bones: bones.clone(),
        skin: joint_names,
        inverse_bind_matrices,
        morph_targets: Vec::new(),
    })
}

/// Fills each vertex channel from the shape buffer, then from the partition
/// buffer only for channels the shape does not carry. A dynamic shape stores
/// live positions in its trailing array (parsed into `vertex_data.position`)
/// while normals, UVs and triangles come from the partition.
fn legacy_shape_mesh(
    shape: &BSTriShape,
    partition: &NiSkinPartition,
    name: Option<String>,
) -> StaticMesh {
    let mut mesh = tri_shape_to_mesh(shape, name);
    if mesh.positions.is_empty() {
        mesh.positions = partition
            .vertex_data
            .iter()
            .filter_map(|vertex| vertex.position)
            .collect();
    }
    if mesh.normals.is_empty() {
        mesh.normals = partition
            .vertex_data
            .iter()
            .filter_map(|vertex| vertex.normal)
            .collect();
    }
    if mesh.uvs.is_empty() {
        mesh.uvs = partition
            .vertex_data
            .iter()
            .filter_map(|vertex| vertex.uv)
            .collect();
    }
    if mesh.colors.is_empty() {
        mesh.colors = partition
            .vertex_data
            .iter()
            .filter_map(|vertex| vertex.vertex_colors)
            .map(|color| {
                BSVec4(glam::Vec4::new(
                    f32::from(color.x) / 255.0,
                    f32::from(color.y) / 255.0,
                    f32::from(color.z) / 255.0,
                    f32::from(color.w) / 255.0,
                ))
            })
            .collect();
    }
    if mesh.triangles.is_empty() {
        mesh.triangles = partition.triangles.clone();
    }
    mesh
}

type RawInfluences = Vec<([u8; 4], [f32; 4])>;

/// What the raw bone indices of a set of influences address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InfluenceIndices {
    /// A shared vertex buffer (the shape's, or the partition's SSE copy)
    /// stores indices into the skin instance's bone list.
    InstanceBones,
    /// The per-partition index arrays store indices into the partition's bone
    /// palette.
    PartitionPalette,
}

fn vertex_influences(
    shape_index: u32,
    block_type: &str,
    shape: &BSTriShape,
    partition_block: u32,
    partition: &NiSkinPartition,
    position_count: usize,
) -> Result<(RawInfluences, InfluenceIndices), String> {
    let buffer = if shape
        .vertex_data
        .iter()
        .any(|vertex| vertex.bone_indices.is_some() || vertex.bone_weights.is_some())
    {
        &shape.vertex_data
    } else {
        &partition.vertex_data
    };

    if buffer
        .iter()
        .any(|vertex| vertex.bone_indices.is_some() || vertex.bone_weights.is_some())
    {
        let mut influences = Vec::with_capacity(buffer.len());
        for (vertex_index, vertex) in buffer.iter().enumerate() {
            let indices = vertex.bone_indices.ok_or_else(|| {
                format!(
                    "shape block {shape_index} ({block_type}) vertex {vertex_index} has skin weights in its vertex buffer but no bone indices"
                )
            })?;
            let weights = vertex.bone_weights.ok_or_else(|| {
                format!(
                    "shape block {shape_index} ({block_type}) vertex {vertex_index} has bone indices in its vertex buffer but no skin weights"
                )
            })?;
            influences.push((
                [indices.x, indices.y, indices.z, indices.w],
                [weights.x, weights.y, weights.z, weights.w],
            ));
        }
        return Ok((influences, InfluenceIndices::InstanceBones));
    }

    if partition.weights.is_empty() || partition.bone_indices.is_empty() {
        return Err(format!(
            "shape block {shape_index} ({block_type}) has no vertex-buffer skin data and NiSkinPartition block {partition_block} has no per-vertex arrays"
        ));
    }
    if partition.weights.len() != partition.bone_indices.len() {
        return Err(format!(
            "shape block {shape_index} ({block_type}): NiSkinPartition block {partition_block} has {} weight rows but {} bone-index rows",
            partition.weights.len(),
            partition.bone_indices.len()
        ));
    }

    // Rows run in partition order, and the concatenated vertex map ties each
    // one to its shape vertex. That holds even when the row count equals the
    // vertex count: the map can still be a permutation. Partitions that repeat
    // shared vertices have more rows than positions.
    if let Some(vertex_map) = partition.vertex_map.as_ref() {
        if vertex_map.len() == partition.weights.len() {
            let mut merged: Vec<Option<([u8; 4], [f32; 4])>> = vec![None; position_count];
            for (row, (row_weights, row_indices)) in partition
                .weights
                .iter()
                .zip(&partition.bone_indices)
                .enumerate()
            {
                let target = usize::from(vertex_map[row]);
                if target >= position_count {
                    return Err(format!(
                        "shape block {shape_index} ({block_type}): NiSkinPartition block {partition_block} vertex map entry {row} points at vertex {target} but the shape has {position_count} positions"
                    ));
                }
                merged[target] = Some(influence_from_row(
                    shape_index,
                    block_type,
                    partition_block,
                    row,
                    row_weights,
                    row_indices,
                )?);
            }
            let mut influences = Vec::with_capacity(position_count);
            for (vertex_index, row) in merged.into_iter().enumerate() {
                let Some(row) = row else {
                    return Err(format!(
                        "shape block {shape_index} ({block_type}): NiSkinPartition block {partition_block} vertex map leaves vertex {vertex_index} without skin data"
                    ));
                };
                influences.push(row);
            }
            return Ok((influences, InfluenceIndices::PartitionPalette));
        }
    }

    // Without a map the rows can only be read in shape-vertex order.
    if partition.vertex_map.is_none() && partition.weights.len() == position_count {
        let mut influences = Vec::with_capacity(position_count);
        for (vertex_index, (row_weights, row_indices)) in partition
            .weights
            .iter()
            .zip(&partition.bone_indices)
            .enumerate()
        {
            influences.push(influence_from_row(
                shape_index,
                block_type,
                partition_block,
                vertex_index,
                row_weights,
                row_indices,
            )?);
        }
        return Ok((influences, InfluenceIndices::PartitionPalette));
    }

    Err(format!(
        "shape block {shape_index} ({block_type}): NiSkinPartition block {partition_block} has {} weighted vertices, {} vertex-map entries and the shape has {position_count} positions",
        partition.weights.len(),
        partition.vertex_map.as_ref().map_or(0, Vec::len)
    ))
}

fn influence_from_row(
    shape_index: u32,
    block_type: &str,
    partition_block: u32,
    vertex_index: usize,
    row_weights: &[f32],
    row_indices: &[u8],
) -> Result<([u8; 4], [f32; 4]), String> {
    if row_weights.len() != row_indices.len() {
        return Err(format!(
            "shape block {shape_index} ({block_type}): NiSkinPartition block {partition_block} vertex {vertex_index} has {} weights but {} bone indices",
            row_weights.len(),
            row_indices.len()
        ));
    }
    if row_weights.len() > 4 {
        return Err(format!(
            "shape block {shape_index} ({block_type}): NiSkinPartition block {partition_block} vertex {vertex_index} has {} influences but glTF stores at most 4",
            row_weights.len()
        ));
    }
    let mut indices = [0u8; 4];
    indices[..row_indices.len()].copy_from_slice(row_indices);
    let mut weights = [0.0f32; 4];
    weights[..row_weights.len()].copy_from_slice(row_weights);
    Ok((indices, weights))
}

fn normalize_vertex_influences(
    raw_joints: [u8; 4],
    raw_weights: [f32; 4],
    palette: Option<&[u16]>,
    bone_count: usize,
    shape_index: u32,
    vertex_index: usize,
) -> Result<(U8Vec4, Vec4), String> {
    let total: f32 = raw_weights.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return Err(format!(
            "shape block {shape_index} vertex {vertex_index} has a skin weight sum of {total}; a zero-weight vertex cannot be skinned"
        ));
    }

    let mut joints = [0u8; 4];
    let mut weights = [0.0f32; 4];
    for slot in 0..4 {
        let weight = raw_weights[slot];
        // Zero-weight slots carry junk indices in retail vertex buffers, so
        // only the influences that actually move the vertex are validated.
        if weight == 0.0 {
            continue;
        }
        if !weight.is_finite() || weight < 0.0 {
            return Err(format!(
                "shape block {shape_index} vertex {vertex_index} has an invalid skin weight {weight}"
            ));
        }
        let raw_index = usize::from(raw_joints[slot]);
        let joint = match palette {
            Some(palette) => usize::from(palette.get(raw_index).copied().ok_or_else(|| {
                format!(
                    "shape block {shape_index} vertex {vertex_index} references palette entry {raw_index} but the palette has {} entries",
                    palette.len()
                )
            })?),
            None => raw_index,
        };
        if joint >= bone_count {
            return Err(format!(
                "shape block {shape_index} vertex {vertex_index} maps to joint {joint} but the skin instance has {bone_count} bones"
            ));
        }
        let joint = u8::try_from(joint).map_err(|_| {
            format!(
                "shape block {shape_index} vertex {vertex_index} maps to joint {joint} which does not fit a U8 glTF joint index"
            )
        })?;
        joints[slot] = joint;
        weights[slot] = weight / total;
    }

    Ok((U8Vec4::from_array(joints), Vec4::from_array(weights)))
}

pub fn tri_shape_to_mesh(tri_shape: &BSTriShape, name: Option<String>) -> StaticMesh {
    let mut mesh = StaticMesh::default();

    mesh.name = name;

    let positions: Vec<Vec3> = tri_shape.get_vertex_positions();
    let triangles = tri_shape.get_triangle_indices();
    let normals: Vec<Vec3> = tri_shape.get_vertex_normals();
    let uvs = tri_shape.get_vertex_uvs();

    mesh.positions.extend(positions);
    mesh.triangles.extend(triangles);
    mesh.normals.extend(normals);
    mesh.uvs.extend(uvs);
    mesh.colors
        .extend(tri_shape.vertex_data.iter().filter_map(|vertex| {
            vertex.vertex_colors.map(|color| {
                BSVec4(glam::Vec4::new(
                    f32::from(color.x) / 255.0,
                    f32::from(color.y) / 255.0,
                    f32::from(color.z) / 255.0,
                    f32::from(color.w) / 255.0,
                ))
            })
        }));

    mesh
}

pub fn legacy_tri_shape_to_mesh(data: &NiTriShapeData, name: Option<String>) -> StaticMesh {
    StaticMesh {
        name,
        positions: data.positions.clone(),
        normals: data.normals.clone(),
        uvs: data.uvs.clone(),
        triangles: data.triangles.clone(),
        colors: data.colors.iter().copied().map(BSVec4).collect(),
    }
}

/// SLSF1 `Model_Space_Normals`: the shape's normal map holds model-space normals, so the
/// geometry itself usually ships none.
const SLSF1_MODEL_SPACE_NORMALS: u32 = 1 << 12;

fn geometry_shader_property(block: &NifBlock) -> Option<u32> {
    match block {
        NifBlock::BSTriShape(shape) => Some(shape.shader_property),
        NifBlock::BSDynamicTriShape(shape) => Some(shape.bs_tri_shape.shader_property),
        NifBlock::BSSubIndexTriShape(shape) => Some(shape.bs_tri_shape.shader_property),
        NifBlock::BSLODTriShape(shape) => Some(shape.bs_tri_shape.shader_property),
        NifBlock::NiTriShape(shape) => Some(shape.shader_property),
        _ => None,
    }
}

/// Whether the geometry block at `block_index` is lit through a model-space normal map.
fn uses_model_space_normals(nif: &NifFile, block_index: u32) -> bool {
    let Some(property) = nif
        .blocks
        .get(block_index as usize)
        .and_then(geometry_shader_property)
    else {
        return false;
    };
    matches!(
        nif.blocks.get(property as usize),
        Some(NifBlock::BSLightingShaderProperty(shader))
            if shader.shader_flags_1.raw() & SLSF1_MODEL_SPACE_NORMALS != 0
    )
}

/// Gives every model-space-normal shape that carries no vertex normals a smooth set.
///
/// Skyrim draws these shapes (bodies, hands, feet, FaceGen heads) entirely from the normal map,
/// so their vertex buffers omit normals. glTF loaders treat a primitive without `NORMAL` as
/// flat-shaded and split every vertex, which faceted the skin. The normals are computed from the
/// same positions the exporter writes, so they sit in the same (shape-local, bind-pose) space
/// and go through the same skin or node transform. Shapes that already have normals, or that
/// use tangent-space maps, are left untouched.
fn fill_model_space_normals(nif: &NifFile, model: &mut Model) {
    for node in &model.static_nodes {
        let Some(mesh) = node
            .mesh
            .and_then(|index| model.static_meshes.get_mut(index))
        else {
            continue;
        };
        if mesh.normals.is_empty() && uses_model_space_normals(nif, node.block_index) {
            mesh.normals = smooth_vertex_normals(&mesh.positions, &mesh.triangles);
        }
    }
    for skeletal in &mut model.skeletal_meshes {
        let (Some(block), Some(mesh)) = (skeletal.source_block, skeletal.mesh.as_mut()) else {
            continue;
        };
        if mesh.normals.is_empty() && uses_model_space_normals(nif, block) {
            mesh.normals = smooth_vertex_normals(&mesh.positions, &mesh.triangles);
        }
    }
}

/// Angle-weighted vertex normals, averaged over every vertex that shares a position.
///
/// Vertices are welded by exact position first: UV seams duplicate a vertex per side, and
/// averaging each copy over only its own triangles would leave a visible crease along every
/// seam. Each face normal contributes in proportion to the corner angle it makes at the vertex
/// (Thürmer and Wüthrich), so the result depends on the surface's shape rather than on how it
/// was triangulated; area weighting lets the long thin triangles that border seams and fill
/// joints pull the normal towards themselves. The face orientation follows the triangle
/// winding, `(b - a) × (c - a)`, the same convention glTF loaders use for flat normals.
pub fn smooth_vertex_normals(positions: &[Vec3], triangles: &[glam::u16::U16Vec3]) -> Vec<Vec3> {
    // `+ 0.0` folds -0.0 into 0.0, so the two zeros weld together.
    let key = |position: Vec3| {
        [
            (position.x + 0.0).to_bits(),
            (position.y + 0.0).to_bits(),
            (position.z + 0.0).to_bits(),
        ]
    };
    let mut group_ids = std::collections::HashMap::new();
    let groups: Vec<usize> = positions
        .iter()
        .map(|position| {
            let next = group_ids.len();
            *group_ids.entry(key(*position)).or_insert(next)
        })
        .collect();

    let mut welded = vec![Vec3::ZERO; group_ids.len()];
    let mut own = vec![Vec3::ZERO; positions.len()];
    for triangle in triangles {
        let corners = [
            usize::from(triangle.x),
            usize::from(triangle.y),
            usize::from(triangle.z),
        ];
        if corners.iter().any(|&index| index >= positions.len()) {
            continue;
        }
        let points = corners.map(|index| positions[index]);
        let face = (points[1] - points[0]).cross(points[2] - points[0]);
        let length = face.length();
        if !length.is_finite() || length <= 0.0 {
            continue;
        }
        let face = face / length;
        for corner in 0..3 {
            let to_next = (points[(corner + 1) % 3] - points[corner]).normalize_or_zero();
            let to_previous = (points[(corner + 2) % 3] - points[corner]).normalize_or_zero();
            let angle = to_next.dot(to_previous).clamp(-1.0, 1.0).acos();
            let index = corners[corner];
            welded[groups[index]] += face * angle;
            own[index] += face * angle;
        }
    }

    // A welded sum can cancel where two opposite-facing sheets touch; fall back to the vertex's
    // own faces, then to +Z for a vertex no drawable triangle uses.
    (0..positions.len())
        .map(|index| {
            welded[groups[index]]
                .try_normalize()
                .or_else(|| own[index].try_normalize())
                .unwrap_or(Vec3::Z)
        })
        .collect()
}

/// Builds a bind-pose/static representation of every geometry block. World
/// rendering can use this when an old skin has no complete glTF skin export.
pub fn nif_to_static_model(nif: &NifFile) -> Result<Model, String> {
    let mut model = Model::default();
    populate_static_scene(nif, &mut model)?;
    model.materials = nif
        .get_materials()
        .into_iter()
        .map(|material| crate::model::all::Material {
            diffuse: material.diffuse.clone(),
            normal: material.normal.clone(),
            glow: material.glow.clone(),
            specular: material.specular.clone(),
        })
        .collect();
    fill_model_space_normals(nif, &mut model);
    model.validate()?;
    Ok(model)
}

/// Adds supported NIF nodes and geometry to the static scene, retaining geometryless template nodes.
fn populate_static_scene(nif: &NifFile, model: &mut Model) -> Result<(), String> {
    let supported_blocks: BTreeSet<u32> = nif
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (block.as_node().is_ok()
                || matches!(
                    block,
                    NifBlock::BSTriShape(_)
                        | NifBlock::BSDynamicTriShape(_)
                        | NifBlock::BSSubIndexTriShape(_)
                        | NifBlock::BSLODTriShape(_)
                        | NifBlock::NiTriShape(_)
                ))
            .then_some(index as u32)
        })
        .collect();
    let distant_lod_blocks = distant_lod_subtree_blocks(nif);

    for (index, block) in nif.blocks.iter().enumerate() {
        let block_index = index as u32;
        match block {
            NifBlock::NiNode(node) | NifBlock::BSFadeNode(node) => {
                model.static_nodes.push(static_node_from_av(
                    nif,
                    block_index,
                    &node.av,
                    node.children
                        .iter()
                        .copied()
                        .filter(|child| supported_blocks.contains(child))
                        .collect(),
                    None,
                ));
            }
            // Node subtypes whose extra fields the static scene does not use.
            NifBlock::BSMultiBoundNode(_)
            | NifBlock::BSOrderedNode(_)
            | NifBlock::NiBillboardNode(_)
            | NifBlock::BSValueNode(_) => {
                let node = block.as_node()?;
                model.static_nodes.push(static_node_from_av(
                    nif,
                    block_index,
                    &node.av,
                    node.children
                        .iter()
                        .copied()
                        .filter(|child| supported_blocks.contains(child))
                        .collect(),
                    None,
                ));
            }
            NifBlock::BSTriShape(shape) => {
                push_modern_static_shape(
                    nif,
                    model,
                    block_index,
                    shape,
                    distant_lod_blocks[index],
                )?;
            }
            NifBlock::BSDynamicTriShape(shape) => {
                push_modern_static_shape(
                    nif,
                    model,
                    block_index,
                    &shape.bs_tri_shape,
                    distant_lod_blocks[index],
                )?;
            }
            NifBlock::BSSubIndexTriShape(shape) => {
                push_modern_static_shape(
                    nif,
                    model,
                    block_index,
                    &shape.bs_tri_shape,
                    distant_lod_blocks[index],
                )?;
            }
            NifBlock::BSLODTriShape(shape) => {
                push_modern_static_shape(
                    nif,
                    model,
                    block_index,
                    &shape.bs_tri_shape,
                    distant_lod_blocks[index],
                )?;
            }
            NifBlock::NiTriShape(shape) => {
                if shape.data == u32::MAX {
                    model.static_nodes.push(StaticSceneNode {
                        block_index,
                        name: nif
                            .header
                            .get_string(shape.name as usize)
                            .ok()
                            .map(str::to_owned),
                        translation: shape.translation,
                        rotation: shape.rotation,
                        scale: shape.scale,
                        children: Vec::new(),
                        mesh: None,
                    });
                    continue;
                }
                let data = nif
                    .blocks
                    .get(shape.data as usize)
                    .and_then(|block| match block {
                        NifBlock::NiTriShapeData(data) => Some(data),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        format!(
                            "NiTriShape block {block_index} references invalid data block {}",
                            shape.data
                        )
                    })?;
                let name = nif
                    .header
                    .get_string(shape.name as usize)
                    .ok()
                    .map(str::to_owned);
                let mesh_index = model.static_meshes.len();
                model
                    .static_meshes
                    .push(legacy_tri_shape_to_mesh(data, name.clone()));
                model.static_nodes.push(StaticSceneNode {
                    block_index,
                    name,
                    translation: shape.translation,
                    rotation: shape.rotation,
                    scale: shape.scale,
                    children: Vec::new(),
                    mesh: Some(mesh_index),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Marks the blocks a distant-LOD container holds: every `BSMultiBoundNode`
/// (the root of the `.btr`/`.bto` meshes under `meshes/terrain/`) and its
/// subtree. Shapes there can carry per-vertex tint, so they keep that
/// attribute. Ordinary world meshes use vertex colours for shader effects the
/// material contract does not model yet, and preserving them there would change
/// how most converted statics render.
pub(crate) fn distant_lod_subtree_blocks(nif: &NifFile) -> Vec<bool> {
    let mut reached = vec![false; nif.blocks.len()];
    let mut pending: Vec<u32> = nif
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            matches!(block, NifBlock::BSMultiBoundNode(_)).then_some(index as u32)
        })
        .collect();
    while let Some(index) = pending.pop() {
        let Some(slot) = reached.get_mut(index as usize) else {
            continue;
        };
        if std::mem::replace(slot, true) {
            continue;
        }
        if let Ok(node) = nif.blocks[index as usize].as_node() {
            pending.extend(node.children.iter().copied());
        }
    }
    reached
}

/// Adds an SSE shape to the static scene, recovering missing geometry from its skin partition.
fn push_modern_static_shape(
    nif: &NifFile,
    model: &mut Model,
    block_index: u32,
    shape: &BSTriShape,
    preserve_vertex_colors: bool,
) -> Result<(), String> {
    let name = nif
        .header
        .get_string(shape.av.object.name as usize)
        .ok()
        .map(str::to_owned);
    let mesh_index = model.static_meshes.len();
    let mut mesh = tri_shape_to_mesh(shape, name.clone());

    if preserve_vertex_colors {
        mesh.colors = shape
            .vertex_data
            .iter()
            .filter_map(|vertex| vertex.vertex_colors)
            .map(|color| {
                BSVec4(glam::Vec4::new(
                    f32::from(color.x) / 255.0,
                    f32::from(color.y) / 255.0,
                    f32::from(color.z) / 255.0,
                    f32::from(color.w) / 255.0,
                ))
            })
            .collect();
    }
    if mesh.positions.is_empty() || mesh.triangles.is_empty() {
        if let Some(partition) = nif
            .blocks
            .get(shape.skin as usize)
            .and_then(|block| match block {
                NifBlock::NiSkinInstance(instance) => nif
                    .blocks
                    .get(instance.skin_partition as usize)
                    .and_then(|block| match block {
                        NifBlock::NiSkinPartition(partition) => Some(partition),
                        _ => None,
                    }),
                _ => None,
            })
        {
            if mesh.positions.is_empty() {
                mesh.positions = partition
                    .vertex_data
                    .iter()
                    .filter_map(|vertex| vertex.position)
                    .collect();
                mesh.normals = partition
                    .vertex_data
                    .iter()
                    .filter_map(|vertex| vertex.normal)
                    .collect();
                mesh.uvs = partition
                    .vertex_data
                    .iter()
                    .filter_map(|vertex| vertex.uv)
                    .collect();
                mesh.colors = partition
                    .vertex_data
                    .iter()
                    .filter_map(|vertex| vertex.vertex_colors)
                    .map(|color| {
                        BSVec4(glam::Vec4::new(
                            f32::from(color.x) / 255.0,
                            f32::from(color.y) / 255.0,
                            f32::from(color.z) / 255.0,
                            f32::from(color.w) / 255.0,
                        ))
                    })
                    .collect();
            }
            mesh.triangles = partition.triangles.clone();
        }
    }
    if mesh.positions.is_empty()
        && shape.num_vertices == 0
        && shape.num_triangles == 0
        && shape.data_size == 0
    {
        warn!(
            "geometry block {block_index} ({name:?}) is an explicit empty shape; preserving it as a scene node without a mesh"
        );
        model.static_nodes.push(static_node_from_av(
            nif,
            block_index,
            &shape.av,
            Vec::new(),
            None,
        ));
        return Ok(());
    }
    if mesh.uvs.iter().any(|uv| !uv.is_finite()) {
        warn!(
            "geometry block {block_index} ({name:?}) contains non-finite UVs; omitting the UV attribute while preserving geometry"
        );
        mesh.uvs.clear();
    }
    let discarded_triangles = sanitize_non_finite_positions(&mut mesh);
    if discarded_triangles != 0 {
        warn!(
            "geometry block {block_index} ({name:?}) contains non-finite positions; discarded {discarded_triangles} affected triangles"
        );
    }
    mesh.validate()
        .map_err(|error| {
            format!(
                "geometry block {block_index}: {error}; descriptor={:?}, vertices={}, triangles={}, data_size={}",
                shape.vertex_desc, shape.num_vertices, shape.num_triangles, shape.data_size
            )
        })?;
    model.static_meshes.push(mesh);
    model.static_nodes.push(static_node_from_av(
        nif,
        block_index,
        &shape.av,
        Vec::new(),
        Some(mesh_index),
    ));
    Ok(())
}

/// Bethesda's shipped effect meshes occasionally contain sentinel NaN/Inf
/// vertices. They are not drawable geometry, but rejecting the whole NIF also
/// discards its valid sibling shapes. Remove only triangles that reference a
/// sentinel and replace the now-unused position with a finite value so model
/// validation and glTF bounds remain well-defined.
fn sanitize_non_finite_positions(mesh: &mut StaticMesh) -> usize {
    let invalid = mesh
        .positions
        .iter()
        .map(|position| !position.is_finite())
        .collect::<Vec<_>>();
    if !invalid.iter().any(|value| *value) {
        return 0;
    }

    for (position, is_invalid) in mesh.positions.iter_mut().zip(&invalid) {
        if *is_invalid {
            *position = Vec3::ZERO;
        }
    }
    let before = mesh.triangles.len();
    mesh.triangles.retain(|triangle| {
        [triangle.x, triangle.y, triangle.z]
            .into_iter()
            .all(|index| !invalid.get(usize::from(index)).copied().unwrap_or(true))
    });
    before - mesh.triangles.len()
}

#[cfg(test)]
mod position_sanitization_tests {
    use super::*;

    #[test]
    fn discards_only_triangles_that_reference_non_finite_positions() {
        let mut mesh = StaticMesh {
            positions: vec![
                Vec3::ZERO,
                Vec3::X,
                Vec3::Y,
                Vec3::new(f32::INFINITY, 0.0, 0.0),
            ],
            triangles: vec![
                glam::u16::U16Vec3::new(0, 1, 2),
                glam::u16::U16Vec3::new(1, 2, 3),
            ],
            ..Default::default()
        };

        assert_eq!(sanitize_non_finite_positions(&mut mesh), 1);
        assert_eq!(mesh.triangles.len(), 1);
        assert!(mesh.positions.iter().all(|position| position.is_finite()));
        mesh.validate().unwrap();
    }
}

fn static_node_from_av(
    nif: &NifFile,
    block_index: u32,
    av: &NiAVObject,
    children: Vec<u32>,
    mesh: Option<usize>,
) -> StaticSceneNode {
    StaticSceneNode {
        block_index,
        name: nif
            .header
            .get_string(av.object.name as usize)
            .ok()
            .map(str::to_owned),
        translation: av.translation.0 .0,
        rotation: av.rotation.0 .0,
        scale: av.scale.0,
        children,
        mesh,
    }
}

pub struct NifFileV3 {
    pub header: NifHeader,
    pub raw_blocks: Vec<NifBlock>,
    pub nodes: BTreeMap<u32, NiNode>,
    pub skins: BTreeMap<u32, BSSkinInstance>,
    pub materials: BTreeMap<u32, BSShaderTextureSet>,
    pub shapes: BTreeMap<u32, BSTriShape>,
    pub seg_shapes: BTreeMap<u32, BSSubIndexTriShape>,
}

impl NifFileV3 {
    pub fn open(path: &str) -> Result<Self, std::io::Error> {
        let mut file = std::fs::File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let (_, nif) = NifFileV3::parse(&buf).unwrap();
        Ok(nif)
    }

    pub fn get_node(&self, index: u32) -> Option<&NiNode> {
        self.nodes.get(&index)
    }

    pub fn get_skin(&self, index: u32) -> Option<&BSSkinInstance> {
        self.skins.get(&index)
    }

    pub fn get_material(&self, index: u32) -> Option<&BSShaderTextureSet> {
        self.materials.get(&index)
    }

    pub fn get_shape(&self, index: u32) -> Option<&BSTriShape> {
        self.shapes.get(&index)
    }

    pub fn get_seg_shape(&self, index: u32) -> Option<&BSSubIndexTriShape> {
        self.seg_shapes.get(&index)
    }

    pub fn get_mat_for_shape(&self, index: u32) -> Option<&BSShaderTextureSet> {
        for (mat_index, mat) in self.materials.iter() {
            if mat_index > &index {
                return Some(mat);
            }
        }

        let (_last_index, last_mat) = self.materials.iter().last().unwrap();
        Some(last_mat)
    }

    pub fn get_mat_id_for_shape(&self, index: u32) -> u32 {
        for (mat_index, _mat) in self.materials.iter() {
            if mat_index > &index {
                return *mat_index;
            }
        }

        let (last_index, _last_mat) = self.materials.iter().last().unwrap();
        *last_index
    }

    pub fn get_node_by_name(&self, name: &str) -> Option<&NiNode> {
        if let Some(name_id) = self
            .header
            .strings
            .iter()
            .position(|string| string.0 == name)
        {
            for node in self.nodes.values() {
                if node.name() == name_id as u32 {
                    return Some(node);
                }
            }
        }

        None
    }

    pub fn has_skin(&self) -> bool {
        !self.skins.is_empty()
    }

    pub fn get_string(&self, index: usize) -> Result<&str, String> {
        self.header.get_string(index)
    }

    pub fn get_mesh_materials_ids(&self) -> Vec<u8> {
        let mut current_mat = -1;

        let mut positions = Vec::new();

        for block in self.raw_blocks.iter() {
            match block {
                NifBlock::BSTriShape(_shape) => {
                    positions.push((current_mat).max(0) as u8);
                }
                NifBlock::BSSubIndexTriShape(_shape) => {
                    positions.push((current_mat).max(0) as u8);
                }
                NifBlock::BSShaderTextureSet(_textures) => {
                    current_mat += 1;
                }
                _ => {}
            }
        }

        positions
    }

    /// Basic hacky function to get materials and textures from the nif file
    /// Returns a tuple with a vector of materials and a set of textures
    /// TODO: There is a ton of room for improvement here, but it works for now
    pub fn build_materials(&self) -> (Vec<super::model::all::Material>, BTreeSet<String>) {
        // Init empty material vector
        let mut materials = Vec::new();

        // Init empty texture set
        let mut textures = BTreeSet::new();

        // Loop through raw blocks for materials
        for block in &self.raw_blocks {
            match block {
                NifBlock::BSShaderTextureSet(set) => {
                    // Init empty material
                    let mut diffuse = None;
                    let mut normal = None;
                    let mut glow = None;
                    let mut specular = None;

                    if let Some(raw_diffuse) = &set.diffuse {
                        let mut fixed = normalize_esm_path(&raw_diffuse);
                        ensure_texture_parent(&mut fixed);

                        textures.insert(fixed.clone());
                        diffuse = Some(fixed);
                    }

                    if let Some(raw_normal) = &set.normal {
                        let mut fixed = normalize_esm_path(&raw_normal);
                        ensure_texture_parent(&mut fixed);

                        textures.insert(fixed.clone());
                        normal = Some(fixed);
                    }

                    if let Some(raw_glow) = &set.glow {
                        let mut fixed = normalize_esm_path(&raw_glow);
                        ensure_texture_parent(&mut fixed);

                        textures.insert(fixed.clone());
                        glow = Some(fixed);
                    }

                    if let Some(raw_specular) = &set.specular {
                        let mut fixed = normalize_esm_path(&raw_specular);
                        ensure_texture_parent(&mut fixed);

                        textures.insert(fixed.clone());
                        specular = Some(fixed);
                    }

                    materials.push(super::model::all::Material {
                        diffuse,
                        normal,
                        glow,
                        specular,
                    });
                }
                _ => {}
            }
        }

        (materials, textures)
    }

    pub fn to_gltf(&self, file_name: String, skeleton: Option<&NifFileV3>) -> (Root, Vec<u8>) {
        let mut root = Root::default();
        let mut bin_data = Vec::new();

        root.extensions_used
            .push("KHR_materials_pbrSpecularGlossiness".to_string());
        root.extensions_required
            .push("KHR_materials_pbrSpecularGlossiness".to_string());

        // Check if the nif file has a skeleton
        if self.has_skin() {
            // Check if skeleton is provided from another file
            if let Some(skel) = skeleton {
                // Use imported nodes
                if push_nodes_to_root(&mut root, &skel).is_err() {
                    debug!("Unknown error: Failed to push nodes to root");
                    panic!("Unknown error: Failed to push nodes to root");
                }
            } else {
                // Use nodes from current file
                if push_nodes_to_root(&mut root, &self).is_err() {
                    debug!("Unknown error: Failed to push nodes to root");
                    panic!("Unknown error: Failed to push nodes to root");
                }
            }

            // Switch back to original nif file, process meshes from nif
            for (index, shape) in &self.seg_shapes {
                let mesh_name =
                    if let Ok(name) = self.get_string(shape.bs_tri_shape.av.object.name as usize) {
                        Some(name.to_string())
                    } else {
                        debug!(
                            "Mesh name lookup failed. Could not find index [{}] in string list.",
                            index
                        );
                        None
                    };

                let mut attributes = BTreeMap::new();

                attributes.insert(
                    Checked::Valid(Semantic::Positions),
                    Index::new(root.accessors.len() as u32),
                );
                push_positions_to_root(
                    &mut root,
                    shape.bs_tri_shape.get_vertices_as_bytes(),
                    &mut bin_data,
                );

                let (min, max) = shape.bs_tri_shape.get_vertices_min_max();
                root.accessors.last_mut().unwrap().min =
                    Some(Value::Array(vec![min.x.into(), min.y.into(), min.z.into()]));
                root.accessors.last_mut().unwrap().max =
                    Some(Value::Array(vec![max.x.into(), max.y.into(), max.z.into()]));

                attributes.insert(
                    Checked::Valid(Semantic::Normals),
                    Index::new(root.accessors.len() as u32),
                );
                push_normals_to_root(
                    &mut root,
                    shape.bs_tri_shape.get_normals_as_bytes(),
                    &mut bin_data,
                );

                attributes.insert(
                    Checked::Valid(Semantic::TexCoords(0)),
                    Index::new(root.accessors.len() as u32),
                );
                push_uvs_to_root(
                    &mut root,
                    shape.bs_tri_shape.get_uvs_as_bytes(),
                    &mut bin_data,
                );

                let indices = Some(Index::new(root.accessors.len() as u32));
                push_indices_to_root(
                    &mut root,
                    shape.bs_tri_shape.get_indices_as_bytes(),
                    &mut bin_data,
                );

                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }

                let weights = shape.bs_tri_shape.get_bone_weights_as_bytes();
                attributes.insert(
                    Checked::Valid(Semantic::Weights(0)),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(weights.len() as u64 / 16),
                    component_type: Checked::Valid(GenericComponentType(ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: Checked::Valid(accessor::Type::Vec4),
                    min: None,
                    max: None,
                    name: Some(format!("WEIGHTS:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(weights.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    name: Some(format!("WEIGHTS:{}", root.meshes.len())),
                    target: Some(Checked::Valid(buffer::Target::ArrayBuffer)),
                    extensions: None,
                    extras: Extras::default(),
                });

                bin_data.extend_from_slice(&weights);

                let joints = shape.bs_tri_shape.get_bone_indices_as_bytes();
                attributes.insert(
                    Checked::Valid(Semantic::Joints(0)),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(joints.len() as u64 / 4),
                    component_type: Checked::Valid(GenericComponentType(ComponentType::U8)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: Checked::Valid(accessor::Type::Vec4),
                    min: None,
                    max: None,
                    name: Some(format!("JOINTS:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(joints.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    name: Some(format!("JOINTS:{}", root.meshes.len())),
                    target: Some(Checked::Valid(buffer::Target::ArrayBuffer)),
                    extensions: None,
                    extras: Extras::default(),
                });

                bin_data.extend_from_slice(&joints);

                let primitives = vec![Primitive {
                    attributes,
                    extensions: None,
                    extras: None,
                    indices,
                    material: Some(Index::new(
                        self.get_mesh_materials_ids()[root.meshes.len()] as u32,
                    )),
                    mode: Checked::Valid(mesh::Mode::Triangles),
                    targets: None,
                }];

                root.nodes.push(Node {
                    camera: None,
                    children: None,
                    extensions: None,
                    extras: None,
                    matrix: None,
                    mesh: Some(Index::new(root.meshes.len() as u32)),
                    name: mesh_name.clone(),
                    rotation: None,
                    scale: None,
                    translation: None,
                    skin: Some(Index::new(root.meshes.len() as u32)),
                    weights: None,
                });

                root.meshes.push(Mesh {
                    extensions: None,
                    extras: None,
                    name: mesh_name,
                    primitives,
                    weights: None,
                });
            }

            for (_block_index, skin) in &self.skins {
                let inverse_bind_matrices = Some(Index::new(root.accessors.len() as u32));
                let mut inverse_bind_data: Vec<u8> = Vec::new();
                let mut joints = Vec::new();

                for joint in skin.bones.iter() {
                    let joint_name = self
                        .get_string(self.get_node(*joint as u32).unwrap().name() as usize)
                        .unwrap();
                    let joint_pos = find_node_in_root(&root, joint_name);
                    let new_joint = Index::new(joint_pos.unwrap() as u32);

                    if !joints.contains(&new_joint) {
                        joints.push(new_joint);
                    }
                }

                match self.raw_blocks[skin.data as usize] {
                    NifBlock::BSSkinBoneData(ref bone_data) => {
                        for bone_transform in &bone_data.bone_list {
                            // let rotation = bone_transform.rotation.to_col_major();

                            // let rotation = Matrix4([
                            //     rotation.0[0],
                            //     rotation.0[1],
                            //     rotation.0[2],
                            //     0.0,
                            //     rotation.0[3],
                            //     rotation.0[4],
                            //     rotation.0[5],
                            //     0.0,
                            //     rotation.0[6],
                            //     rotation.0[7],
                            //     rotation.0[8],
                            //     0.0,
                            //     bone_transform.translation.x,
                            //     bone_transform.translation.y,
                            //     bone_transform.translation.z,
                            //     bone_transform.scale,
                            // ]);

                            inverse_bind_data.extend_from_slice(
                                bone_transform
                                    .into_matrix4()
                                    .0
                                    .to_cols_array()
                                    .as_slice()
                                    .as_bytes(),
                            );
                        }
                    }
                    _ => {
                        panic!("Inverse position block is not a BSSkinBoneData")
                    }
                }

                root.skins.push(gltf::json::Skin {
                    extensions: None,
                    extras: Extras::default(),
                    inverse_bind_matrices,
                    joints,
                    name: None,
                    skeleton: None,
                });

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(inverse_bind_data.len() as u64 / 64),
                    component_type: Checked::Valid(GenericComponentType(ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: Checked::Valid(accessor::Type::Mat4),
                    min: None,
                    max: None,
                    name: Some(format!("INVERSE_BIND_MATRICES:{}", root.skins.len() - 1)),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(inverse_bind_data.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    name: Some(format!("INVERSE_BIND_MATRICES:{}", root.skins.len() - 1)),
                    target: None,
                    extensions: None,
                    extras: Extras::default(),
                });

                bin_data.extend_from_slice(&inverse_bind_data);
            }
        }
        // Process as static mesh
        else {
        }

        root.buffers.push(Buffer {
            byte_length: USize64(bin_data.len() as u64),
            extensions: None,
            extras: None,
            name: None,
            uri: Some(format!("{}.bin", file_name)),
        });

        let (materials, textures) = self.build_materials();

        for texture in &textures {
            root.textures.push(Texture {
                name: None,
                sampler: None,
                source: Index::new(root.images.len() as u32),
                extensions: None,
                extras: Extras::default(),
            });

            root.images.push(Image {
                buffer_view: None,
                mime_type: None,
                name: None,
                uri: Some(texture.clone().replace(".dds", ".png")),
                extensions: None,
                extras: Extras::default(),
            });
        }

        for material in &materials {
            let mut diffuse_texture = None;
            let mut normal_texture = None;
            let mut emissive_texture = None;
            let mut specular_glossiness_texture = None;

            if let Some(d) = &material.diffuse {
                diffuse_texture = Some(Info {
                    index: Index::new(textures.iter().position(|x| x == d).unwrap() as u32),
                    tex_coord: 0,
                    extensions: None,
                    extras: Extras::default(),
                });
            }

            if let Some(n) = &material.normal {
                normal_texture = Some(NormalTexture {
                    index: Index::new(textures.iter().position(|x| x == n).unwrap() as u32),
                    scale: 1.0,
                    tex_coord: 0,
                    extensions: None,
                    extras: Extras::default(),
                });
            }

            if let Some(e) = &material.glow {
                emissive_texture = Some(Info {
                    index: Index::new(textures.iter().position(|x| x == e).unwrap() as u32),
                    tex_coord: 0,
                    extensions: None,
                    extras: Extras::default(),
                });
            }

            if let Some(s) = &material.specular {
                specular_glossiness_texture = Some(Info {
                    index: Index::new(textures.iter().position(|x| x == s).unwrap() as u32),
                    tex_coord: 0,
                    extensions: None,
                    extras: Extras::default(),
                });
            }

            root.materials.push(gltf::json::Material {
                alpha_cutoff: None,
                alpha_mode: Checked::Valid(material::AlphaMode::Opaque),
                double_sided: false,
                name: None,
                pbr_metallic_roughness: PbrMetallicRoughness {
                    base_color_factor: PbrBaseColorFactor::default(),
                    base_color_texture: None,
                    metallic_factor: StrengthFactor::default(),
                    roughness_factor: StrengthFactor::default(),
                    metallic_roughness_texture: None,
                    extensions: None,
                    extras: Extras::default(),
                },
                normal_texture,
                occlusion_texture: None,
                emissive_texture,
                emissive_factor: EmissiveFactor::default(),
                extensions: Some(gltf::json::extensions::material::Material {
                    pbr_specular_glossiness: Some(PbrSpecularGlossiness {
                        diffuse_factor: PbrDiffuseFactor::default(),
                        diffuse_texture,
                        specular_factor: PbrSpecularFactor::default(),
                        glossiness_factor: StrengthFactor::default(),
                        specular_glossiness_texture,
                        others: serde_json::Map::new(),
                        extras: Extras::default(),
                    }),
                    others: serde_json::Map::new(),
                    ..Default::default()
                }),
                extras: Extras::default(),
            });
        }

        let root_node = find_node_in_root(&root, "SkeletonExport").unwrap();

        root.scenes.push(Scene {
            extensions: None,
            extras: Extras::default(),
            name: None,
            nodes: vec![Index::new(root_node as u32)],
        });

        root.scene = Some(Index::new(0));

        (root, bin_data)
    }
}

impl Parse<&[u8]> for NifFileV3 {
    /// Parses version-aware NIF blocks and organizes nodes, skins, materials, and shapes into lookup maps.
    fn parse(i: &[u8]) -> IResult<&[u8], Self, nom::error::Error<&[u8]>> {
        let (i, header) = NifHeader::parse(i)?;

        //println!("NIF Header: {:#?}", header);

        println!("Parsing NIF v3 file with {} blocks.", header.block_count);
        let mut raw_blocks = Vec::new();
        let mut data = i;

        for index in 0..header.block_count as usize {
            println!(
                "Parsing block {}/{} as {}",
                index,
                header.block_count,
                header.get_block_type(index).unwrap().to_string()
            );
            let (i, raw) = nom::bytes::complete::take(header.block_size_index[index])(data)?;
            data = i;
            let (_, block) = NifBlock::parse_with_version(
                raw,
                header.get_block_type(index).unwrap().to_string(),
                header.bethesda_version,
            )?;
            println!(
                "Parsed block type: {:?}",
                header.get_block_type(index).unwrap().to_string()
            );
            raw_blocks.push(block);
        }

        println!("Parsing nodes...");
        let mut nodes = BTreeMap::new();

        for (index, block) in raw_blocks.iter().enumerate() {
            match block {
                NifBlock::NiNode(node) => {
                    nodes.insert(index as u32, node.clone());
                }
                _ => {}
            }
        }

        let mut skins = BTreeMap::new();

        for (index, block) in raw_blocks.iter().enumerate() {
            match block {
                NifBlock::BSSkinInstance(skin) => {
                    skins.insert(index as u32, skin.clone());
                }
                _ => {}
            }
        }

        let mut materials = BTreeMap::new();

        for (index, block) in raw_blocks.iter().enumerate() {
            match block {
                NifBlock::BSShaderTextureSet(material) => {
                    materials.insert(index as u32, material.clone());
                }
                _ => {}
            }
        }

        let mut shapes = BTreeMap::new();

        for (index, block) in raw_blocks.iter().enumerate() {
            match block {
                NifBlock::BSTriShape(shape) => {
                    shapes.insert(index as u32, shape.clone());
                }
                _ => {}
            }
        }

        let mut seg_shapes = BTreeMap::new();

        for (index, block) in raw_blocks.iter().enumerate() {
            match block {
                NifBlock::BSSubIndexTriShape(shape) => {
                    seg_shapes.insert(index as u32, shape.clone());
                }
                _ => {}
            }
        }

        Ok((
            i,
            Self {
                header,
                raw_blocks,
                nodes,
                skins,
                materials,
                shapes,
                seg_shapes,
            },
        ))
    }
}

impl std::fmt::Debug for NifFileV3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NifFileV3\nHeader: {:#?}\nNodes: {}\nSkins: {}\nMaterials: {}\nShapes: {}\nSegShapes: {}", self.header, self.nodes.len(), self.skins.len(), self.materials.len(), self.shapes.len(), self.seg_shapes.len())
    }
}

pub fn find_node_in_root(root: &Root, name: &str) -> Option<usize> {
    for (index, node) in root.nodes.iter().enumerate() {
        if let Some(node_name) = &node.name {
            if node_name == name {
                return Some(index);
            }
        }
    }

    None
}

pub fn push_nodes_to_root(root: &mut Root, nif: &NifFileV3) -> Result<(), String> {
    for (index, node) in &nif.nodes {
        let name;

        if let Ok(node_name) = nif.get_string(node.av.object.name as usize) {
            name = Some(node_name.to_string());
        } else {
            debug!(
                "Node name lookup failed. Could not find index [{}] in string list.",
                index
            );
            panic!(
                "Node name lookup failed. Could not find index [{}] in string list.",
                index
            );
        }

        let scale = if node.av.scale.0 == 1.0 {
            None
        } else {
            Some([node.av.scale.0, node.av.scale.0, node.av.scale.0])
        };

        let translation = if node.av.translation == NifTranslation::default() {
            None
        } else {
            Some([
                node.av.translation.0 .0.x,
                node.av.translation.0 .0.y,
                node.av.translation.0 .0.z,
            ])
        };

        let rotation = if node.av.rotation == NifRotation::default() {
            None
        } else {
            let q = glam::Quat::from_mat3(&node.av.rotation.0 .0);

            Some(UnitQuaternion([q.x, q.y, q.z, q.w]))

            // let q = Quaternion::from(node.av.rotation.0.to_col_major());
            // Some(q.into())
        };

        root.nodes.push(Node {
            camera: None,
            children: None,
            extensions: None,
            extras: None,
            matrix: None,
            mesh: None,
            name,
            rotation,
            scale,
            translation,
            skin: None,
            weights: None,
        });
    }

    // Update the child references now that the nodes are added
    // TODO: Verify the nif files to see if the indices always start at block zero, or if they can be offset,
    //       then merge this loop with the previous one
    for (index, (_index, skel_node)) in nif.nodes.iter().enumerate() {
        if skel_node.children.len() > 0 {
            let children = skel_node
                .children
                .iter()
                .map(|child_index| {
                    nif.get_string(nif.nodes.get(child_index).unwrap().av.object.name as usize)
                        .unwrap()
                        .to_string()
                })
                .collect::<Vec<String>>();

            let mut child_indices = Vec::new();

            for child in children {
                if let Some(child_index) = find_node_in_root(&root, &child) {
                    child_indices.push(Index::new(child_index as u32));
                } else {
                    panic!("Child node not found in root");
                }
            }

            root.nodes[index].children = Some(child_indices);
        }
    }

    Ok(())
}

pub fn push_positions_to_root(root: &mut Root, positions: Vec<u8>, bin_data: &mut Vec<u8>) {
    root.accessors.push(Accessor {
        buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
        byte_offset: None,
        count: USize64(positions.len() as u64 / 12),
        component_type: Checked::Valid(GenericComponentType(ComponentType::F32)),
        extensions: None,
        extras: Extras::default(),
        type_: Checked::Valid(accessor::Type::Vec3),
        min: None,
        max: None,
        name: Some(format!("POSITIONS:{}", root.meshes.len())),
        normalized: false,
        sparse: None,
    });

    root.buffer_views.push(View {
        buffer: Index::new(0),
        byte_length: USize64(positions.len() as u64),
        byte_offset: Some(USize64(bin_data.len() as u64)),
        byte_stride: None,
        name: Some(format!("POSITIONS:{}", root.meshes.len())),
        target: Some(Checked::Valid(buffer::Target::ArrayBuffer)),
        extensions: None,
        extras: Extras::default(),
    });

    bin_data.extend_from_slice(&positions);
}

pub fn push_normals_to_root(root: &mut Root, normals: Vec<u8>, bin_data: &mut Vec<u8>) {
    root.accessors.push(Accessor {
        buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
        byte_offset: None,
        count: USize64(normals.len() as u64 / 12),
        component_type: Checked::Valid(GenericComponentType(ComponentType::F32)),
        extensions: None,
        extras: Extras::default(),
        type_: Checked::Valid(accessor::Type::Vec3),
        min: None,
        max: None,
        name: Some(format!("NORMALS:{}", root.meshes.len())),
        normalized: false,
        sparse: None,
    });

    root.buffer_views.push(View {
        buffer: Index::new(0),
        byte_length: USize64(normals.len() as u64),
        byte_offset: Some(USize64(bin_data.len() as u64)),
        byte_stride: None,
        name: Some(format!("NORMALS:{}", root.meshes.len())),
        target: Some(Checked::Valid(buffer::Target::ArrayBuffer)),
        extensions: None,
        extras: Extras::default(),
    });

    bin_data.extend_from_slice(&normals);
}

pub fn push_uvs_to_root(root: &mut Root, uvs: Vec<u8>, bin_data: &mut Vec<u8>) {
    root.accessors.push(Accessor {
        buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
        byte_offset: None,
        count: USize64(uvs.len() as u64 / 8),
        component_type: Checked::Valid(GenericComponentType(ComponentType::F32)),
        extensions: None,
        extras: Extras::default(),
        type_: Checked::Valid(accessor::Type::Vec2),
        min: None,
        max: None,
        name: Some(format!("UVS:{}", root.meshes.len())),
        normalized: false,
        sparse: None,
    });

    root.buffer_views.push(View {
        buffer: Index::new(0),
        byte_length: USize64(uvs.len() as u64),
        byte_offset: Some(USize64(bin_data.len() as u64)),
        byte_stride: None,
        name: Some(format!("UVS:{}", root.meshes.len())),
        target: Some(Checked::Valid(buffer::Target::ArrayBuffer)),
        extensions: None,
        extras: Extras::default(),
    });

    bin_data.extend_from_slice(&uvs);
}

pub fn push_indices_to_root(root: &mut Root, indices: Vec<u8>, bin_data: &mut Vec<u8>) {
    root.accessors.push(Accessor {
        buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
        byte_offset: None,
        count: USize64(indices.len() as u64 / 2),
        component_type: Checked::Valid(GenericComponentType(ComponentType::U16)),
        extensions: None,
        extras: Extras::default(),
        type_: Checked::Valid(accessor::Type::Scalar),
        min: None,
        max: None,
        name: Some(format!("INDICES:{}", root.meshes.len())),
        normalized: false,
        sparse: None,
    });

    root.buffer_views.push(View {
        buffer: Index::new(0),
        byte_length: USize64(indices.len() as u64),
        byte_offset: Some(USize64(bin_data.len() as u64)),
        byte_stride: None,
        name: Some(format!("INDICES:{}", root.meshes.len())),
        target: Some(Checked::Valid(buffer::Target::ElementArrayBuffer)),
        extensions: None,
        extras: Extras::default(),
    });

    bin_data.extend_from_slice(&indices);
}

pub fn apply_ue5_mods(root: &mut Root) {
    let root_count = root.nodes.len();
    let nodes_copy = root.nodes.clone();

    let mut first = true;

    for skin in root.skins.iter_mut() {
        if first {
            first = false;
            continue;
        }

        root.nodes.extend_from_slice(&nodes_copy);

        let mut joints = Vec::new();

        for joint in skin.joints.iter() {
            let new_joint = joint.value();
            joints.push(Index::new(new_joint as u32 + root_count as u32));
        }

        skin.joints = joints;
    }
}

#[cfg(test)]
mod smooth_normal_tests {
    use super::*;
    use glam::u16::U16Vec3;

    fn assert_close(actual: Vec3, expected: Vec3) {
        assert!(
            (actual - expected).length() < 1e-5,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn welds_split_uv_vertices_into_one_normal() {
        // Two faces folded 90 degrees along the X axis. Each face owns its copy of the shared
        // edge, as a UV seam would leave it.
        let positions = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
        ];
        let triangles = vec![U16Vec3::new(0, 1, 2), U16Vec3::new(3, 4, 5)];

        let normals = smooth_vertex_normals(&positions, &triangles);

        assert_eq!(normals.len(), positions.len());
        assert_close(normals[0], normals[3]);
        assert_close(normals[1], normals[5]);
        // Both faces meet at 90 degrees at the origin, so the welded normal bisects them.
        assert_close(normals[0], Vec3::new(0.0, 1.0, 1.0).normalize());
        assert_close(normals[1], Vec3::new(0.0, 1.0, 1.0).normalize());
        // The unshared corners keep their own face's normal.
        assert_close(normals[2], Vec3::Z);
        assert_close(normals[4], Vec3::Y);
    }

    #[test]
    fn weights_faces_by_corner_angle_not_area() {
        // A vertex shared by a large and a small face, with equal 90 degree corners. Area
        // weighting would lean towards the large face; angle weighting bisects them.
        let positions = vec![
            Vec3::ZERO,
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::new(0.0, 0.0, 0.1),
            Vec3::new(0.1, 0.0, 0.0),
        ];
        let triangles = vec![U16Vec3::new(0, 1, 2), U16Vec3::new(0, 3, 4)];

        let normals = smooth_vertex_normals(&positions, &triangles);

        assert_close(normals[0], Vec3::new(0.0, 1.0, 1.0).normalize());
    }

    #[test]
    fn normals_follow_the_winding_and_stay_unit_length() {
        let positions = vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(5.0, 5.0, 5.0)];
        let triangles = vec![U16Vec3::new(0, 2, 1)];

        let normals = smooth_vertex_normals(&positions, &triangles);

        assert_close(normals[0], -Vec3::Z);
        // A vertex no triangle uses still gets a valid unit normal.
        assert_close(normals[3], Vec3::Z);
        assert!(normals
            .iter()
            .all(|normal| (normal.length() - 1.0).abs() < 1e-5));
    }

    #[test]
    fn opposite_sheets_that_touch_keep_their_own_side() {
        // The same triangle twice with opposite winding: the welded sum cancels.
        let positions = vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::ZERO, Vec3::X, Vec3::Y];
        let triangles = vec![U16Vec3::new(0, 1, 2), U16Vec3::new(3, 5, 4)];

        let normals = smooth_vertex_normals(&positions, &triangles);

        assert_close(normals[0], Vec3::Z);
        assert_close(normals[3], -Vec3::Z);
    }
}
