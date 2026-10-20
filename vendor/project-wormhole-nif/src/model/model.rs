use gltf::json::{
    accessor::GenericComponentType,
    buffer::View,
    extensions::material::{PbrDiffuseFactor, PbrSpecularFactor},
    material::{
        EmissiveFactor, NormalTexture, PbrBaseColorFactor, PbrMetallicRoughness, StrengthFactor,
    },
    scene::UnitQuaternion,
    texture::Info,
    validation::USize64,
};
use nom::{AsBytes, Map};
use project_wormhole_shared::glam;

use super::static_mesh::StaticSceneNode;
use crate::dev::*;
use std::{io::Write, primitive};

pub struct Model {
    pub name: Option<String>,
    pub static_meshes: Vec<super::all::StaticMesh>,
    pub static_nodes: Vec<super::all::StaticSceneNode>,
    pub skeletal_meshes: Vec<super::all::SkeletalMesh>,
    pub materials: Vec<Material>,
    pub material_indices: Vec<u8>,
    pub scene_root_rotation: Option<[f32; 4]>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Model: {}",
            self.name.as_ref().unwrap_or(&"Unnamed".to_string())
        )?;
        writeln!(f, "Static Meshes: {}", self.static_meshes.len())?;
        writeln!(f, "Skeletal Meshes: {}", self.skeletal_meshes.len())?;
        writeln!(f, "Materials: {}", self.materials.len())?;
        Ok(())
    }
}

impl Model {
    pub fn validate(&self) -> Result<(), String> {
        for mesh in &self.static_meshes {
            mesh.validate()?;
        }

        validate_static_scene(&self.static_nodes, self.static_meshes.len())?;

        for mesh in &self.skeletal_meshes {
            mesh.validate()?;
        }

        Ok(())
    }

    pub fn to_gltf(&self, name: String) -> Result<(gltf::json::Root, Vec<u8>), String> {
        use gltf::json::*;

        let mut unresolved = Vec::new();
        for (index, smesh) in self.skeletal_meshes.iter().enumerate() {
            for joint in &smesh.skin {
                if !smesh.bones.bones.contains_key(joint.as_str()) {
                    unresolved.push(format!("{joint}:{index}"));
                }
            }
        }
        if !unresolved.is_empty() {
            unresolved.sort();
            unresolved.dedup();
            return Err(format!(
                "skin joints could not be resolved against the bone tree: {}",
                unresolved.join(", ")
            ));
        }

        let mut root = gltf::json::Root::default();

        root.scene = Some(Index::new(0));
        root.extensions_used = vec!["KHR_materials_pbrSpecularGlossiness".to_string()];

        root.scenes.push(Scene {
            extensions: None,
            extras: Extras::default(),
            name: None,
            nodes: Vec::new(),
        });

        root.buffers.push(Buffer {
            byte_length: USize64(0),
            name: None,
            uri: Some(format!("{}.bin", name)),
            extensions: None,
            extras: Extras::default(),
        });

        let mut bin_data = Vec::new();

        // The upstream exporter only emitted skeletal meshes. Skyrim's world
        // geometry (buildings, rocks, trees, clutter) is overwhelmingly made
        // of static BSTriShape blocks, so silently skipping this collection
        // produced a valid-looking but empty GLB. Emit one glTF mesh/node per
        // static shape and keep every buffer view four-byte aligned.
        for (index, smesh) in self.static_meshes.iter().enumerate() {
            let mut attributes = BTreeMap::new();
            let positions = smesh.positions_as_bytes();
            let normals = smesh.normals_as_bytes();
            let uvs = smesh.uvs_as_bytes();
            let triangles = smesh.triangles_as_bytes();
            let colors = smesh.colors_as_bytes();

            let mut append_attribute = |bytes: &[u8],
                                        component_type,
                                        type_,
                                        count: u64,
                                        semantic: gltf::Semantic,
                                        label: &str| {
                let accessor = Index::new(root.accessors.len() as u32);
                attributes.insert(validation::Checked::Valid(semantic), accessor);
                let (min, max) = if label == "POSITIONS" {
                    let (min, max) = smesh.positions_min_max();
                    (
                        Some(Value::Array(vec![min.x.into(), min.y.into(), min.z.into()])),
                        Some(Value::Array(vec![max.x.into(), max.y.into(), max.z.into()])),
                    )
                } else {
                    (None, None)
                };
                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(count),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        component_type,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(type_),
                    min,
                    max,
                    name: Some(format!("{} | MESH:{}", label, index)),
                    normalized: false,
                    sparse: None,
                });
                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(bytes.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });
                bin_data.extend_from_slice(bytes);
                while bin_data.len() % 4 != 0 {
                    bin_data.push(0);
                }
            };

            if !positions.is_empty() {
                append_attribute(
                    &positions,
                    accessor::ComponentType::F32,
                    accessor::Type::Vec3,
                    (positions.len() / 12) as u64,
                    gltf::Semantic::Positions,
                    "POSITIONS",
                );
            }
            if !normals.is_empty() {
                append_attribute(
                    &normals,
                    accessor::ComponentType::F32,
                    accessor::Type::Vec3,
                    (normals.len() / 12) as u64,
                    gltf::Semantic::Normals,
                    "NORMALS",
                );
            }
            if !uvs.is_empty() {
                append_attribute(
                    &uvs,
                    accessor::ComponentType::F32,
                    accessor::Type::Vec2,
                    (uvs.len() / 8) as u64,
                    gltf::Semantic::TexCoords(0),
                    "UVS",
                );
            }
            if !colors.is_empty() {
                append_attribute(
                    &colors,
                    accessor::ComponentType::F32,
                    accessor::Type::Vec4,
                    (colors.len() / 16) as u64,
                    gltf::Semantic::Colors(0),
                    "COLORS",
                );
            }
            drop(append_attribute);

            let indices = if triangles.is_empty() {
                None
            } else {
                let accessor = Index::new(root.accessors.len() as u32);
                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64((triangles.len() / 2) as u64),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::U16,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Scalar),
                    min: None,
                    max: None,
                    name: Some(format!("TRIANGLES | MESH:{}", index)),
                    normalized: false,
                    sparse: None,
                });
                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(triangles.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });
                bin_data.extend_from_slice(&triangles);
                while bin_data.len() % 4 != 0 {
                    bin_data.push(0);
                }
                Some(accessor)
            };

            let material = if self.materials.is_empty() {
                None
            } else {
                Some(Index::new(index.min(self.materials.len() - 1) as u32))
            };
            let mesh_index = Index::new(root.meshes.len() as u32);
            root.meshes.push(Mesh {
                extensions: None,
                extras: Extras::default(),
                name: smesh.name.clone(),
                primitives: vec![gltf::json::mesh::Primitive {
                    attributes,
                    extensions: None,
                    extras: Extras::default(),
                    indices,
                    material,
                    mode: validation::Checked::Valid(mesh::Mode::Triangles),
                    targets: None,
                }],
                weights: None,
            });
            if self.static_nodes.is_empty() {
                let node_index = Index::new(root.nodes.len() as u32);
                root.nodes.push(Node {
                    camera: None,
                    children: None,
                    extensions: None,
                    extras: Extras::default(),
                    matrix: None,
                    mesh: Some(mesh_index),
                    name: smesh.name.clone(),
                    rotation: None,
                    scale: None,
                    translation: None,
                    skin: None,
                    weights: None,
                });
                root.scenes[0].nodes.push(node_index);
            }
        }

        if !self.static_nodes.is_empty() {
            let first_node = root.nodes.len() as u32;
            let node_indices: BTreeMap<u32, Index<Node>> = self
                .static_nodes
                .iter()
                .enumerate()
                .map(|(offset, node)| {
                    (
                        node.block_index,
                        Index::new(first_node + u32::try_from(offset).unwrap()),
                    )
                })
                .collect();
            let child_blocks: HashSet<u32> = self
                .static_nodes
                .iter()
                .flat_map(|node| node.children.iter().copied())
                .collect();

            for node in &self.static_nodes {
                let quaternion = glam::Quat::from_mat3(&node.rotation).normalize();
                root.nodes.push(Node {
                    camera: None,
                    children: (!node.children.is_empty()).then(|| {
                        node.children
                            .iter()
                            .map(|child| *node_indices.get(child).unwrap())
                            .collect()
                    }),
                    extensions: None,
                    extras: Extras::default(),
                    matrix: None,
                    mesh: node.mesh.map(|mesh| Index::new(mesh as u32)),
                    name: node.name.clone(),
                    rotation: (quaternion != glam::Quat::IDENTITY).then_some(UnitQuaternion([
                        quaternion.x,
                        quaternion.y,
                        quaternion.z,
                        quaternion.w,
                    ])),
                    scale: (node.scale != 1.0).then_some([node.scale; 3]),
                    translation: (node.translation != glam::Vec3::ZERO)
                        .then_some(node.translation.to_array()),
                    skin: None,
                    weights: None,
                });
            }
            root.scenes[0].nodes.extend(
                self.static_nodes
                    .iter()
                    .filter(|node| !child_blocks.contains(&node.block_index))
                    .map(|node| *node_indices.get(&node.block_index).unwrap()),
            );
        }

        for (index, smesh) in self.skeletal_meshes.iter().enumerate() {
            for (name, bone) in &smesh.bones.bones {
                let t = bone.transform.get_translation();
                let r = bone.transform.get_rotation();
                let s = bone.transform.get_scale();

                let translation = if t.is_some() {
                    Some(t.unwrap().0.to_array())
                    //Some(t.unwrap().clone().into())
                } else {
                    None
                };

                let rotation = if r.is_some() {
                    let q = glam::Quat::from_mat3(&r.unwrap().0);
                    Some(UnitQuaternion([q.x, q.y, q.z, q.w]))
                } else {
                    None
                };

                let scale = if s.is_some() {
                    Some([s.unwrap(), s.unwrap(), s.unwrap()])
                } else {
                    None
                };

                root.nodes.push(Node {
                    camera: None,
                    children: None,
                    extensions: None,
                    extras: Extras::default(),
                    matrix: None,
                    mesh: None,
                    name: Some(format!("{}:{}", name, index)),
                    rotation,
                    scale,
                    translation,
                    skin: None,
                    weights: None,
                });
            }

            for (name, bone) in &smesh.bones.bones {
                for bone_child in bone.children.iter() {
                    let parent_target = format!("{}:{}", name, index);

                    debug!("Parent: {} Child: {}", parent_target, bone_child);
                    if let Some(parent) = root.nodes.iter().position(|x| {
                        x.name
                            .as_ref()
                            .is_some_and(|s| s.to_string() == format!("{}:{}", name, index))
                    }) {
                        let child = root
                            .nodes
                            .iter()
                            .position(|x| {
                                x.name.as_ref().is_some_and(|s| {
                                    s.to_string() == format!("{}:{}", bone_child, index)
                                })
                            })
                            .unwrap();

                        if root.nodes[parent].children.is_none() {
                            root.nodes[parent].children = Some(Vec::new());
                        }

                        root.nodes[parent]
                            .children
                            .as_mut()
                            .unwrap()
                            .push(Index::new(child as u32));
                    } else {
                        panic!("Bone {} not found", name);
                    }
                }
            }

            let mut joints = Vec::new();

            //debug!("Skin: {:?}", smesh.skin);

            for joint in &smesh.skin {
                if let Some(bone) = root.nodes.iter().position(|x| {
                    x.name
                        .as_ref()
                        .is_some_and(|s| s.to_string() == format!("{}:{}", joint, index))
                }) {
                    joints.push(Index::new(bone as u32));
                    //warn!("Joint {}:{} found", joint, index);
                } else {
                    return Err(format!(
                        "skin joint {joint}:{index} is missing from the exported node tree"
                    ));
                }
            }

            let inverse_bind_matrices = Some(Index::new(root.accessors.len() as u32));
            let ibm = smesh.inverse_bind_matrices_as_bytes();

            root.accessors.push(Accessor {
                buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                byte_offset: None,
                count: USize64(ibm.len() as u64 / 64),
                component_type: validation::Checked::Valid(GenericComponentType(
                    accessor::ComponentType::F32,
                )),
                extensions: None,
                extras: Extras::default(),
                type_: validation::Checked::Valid(accessor::Type::Mat4),
                min: None,
                max: None,
                name: Some(format!("INVERSE_BIND_MATRICES | MESH:{}", index)),
                normalized: false,
                sparse: None,
            });

            root.buffer_views.push(View {
                buffer: Index::new(0),
                byte_length: USize64(ibm.len() as u64),
                byte_offset: Some(USize64(bin_data.len() as u64)),
                byte_stride: None,
                extensions: None,
                extras: Extras::default(),
                name: None,
                target: None,
            });

            bin_data.extend_from_slice(&ibm);

            if bin_data.len() % 4 != 0 {
                let padding = 4 - (bin_data.len() % 4);
                bin_data.extend_from_slice(&vec![0; padding]);
            }

            root.skins.push(Skin {
                extensions: None,
                extras: Extras::default(),
                inverse_bind_matrices,
                joints,
                name: None,
                skeleton: None,
            });

            let mut attributes = BTreeMap::new();

            let positions = smesh.mesh.as_ref().unwrap().positions_as_bytes();
            let (min, max) = smesh.mesh.as_ref().unwrap().positions_min_max();
            let normals = smesh.mesh.as_ref().unwrap().normals_as_bytes();
            let uvs = smesh.mesh.as_ref().unwrap().uvs_as_bytes();
            let triangles = smesh.mesh.as_ref().unwrap().triangles_as_bytes();
            let colors = smesh.mesh.as_ref().unwrap().colors_as_bytes();
            let weights = smesh.weights_as_bytes();
            let joints = smesh.joints_as_bytes();

            if positions.len() > 0 {
                attributes.insert(
                    validation::Checked::Valid(gltf::Semantic::Positions),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(positions.len() as u64 / 12),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::F32,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec3),
                    min: Some(Value::Array(vec![min.x.into(), min.y.into(), min.z.into()])),
                    max: Some(Value::Array(vec![max.x.into(), max.y.into(), max.z.into()])),
                    name: Some(format!("POSITIONS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(positions.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&positions);

                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }
            }

            if normals.len() > 0 {
                attributes.insert(
                    validation::Checked::Valid(gltf::Semantic::Normals),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(normals.len() as u64 / 12),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::F32,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec3),
                    min: None,
                    max: None,
                    name: Some(format!("NORMALS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(normals.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&normals);

                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }
            }

            if uvs.len() > 0 {
                attributes.insert(
                    validation::Checked::Valid(gltf::Semantic::TexCoords(0)),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(uvs.len() as u64 / 8),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::F32,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec2),
                    min: None,
                    max: None,
                    name: Some(format!("UVS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(uvs.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&uvs);

                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }
            }

            let mut primitives = gltf::json::mesh::Primitive {
                attributes,
                extensions: None,
                extras: Extras::default(),
                indices: None,
                material: Some(Index::new(self.material_indices[index] as u32)),
                mode: validation::Checked::Valid(mesh::Mode::Triangles),
                targets: None,
            };

            if triangles.len() > 0 {
                primitives.indices = Some(Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(triangles.len() as u64 / 2),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::U16,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Scalar),
                    min: None,
                    max: None,
                    name: Some(format!("TRIANGLES | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(triangles.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&triangles);

                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }
            }

            if colors.len() > 0 {
                primitives.attributes.insert(
                    validation::Checked::Valid(gltf::Semantic::Colors(0)),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(colors.len() as u64 / 16),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::F32,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec4),
                    min: None,
                    max: None,
                    name: Some(format!("COLORS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(colors.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&colors);

                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }
            }

            if weights.len() > 0 && joints.len() > 0 {
                primitives.attributes.insert(
                    validation::Checked::Valid(gltf::Semantic::Weights(0)),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(weights.len() as u64 / 16),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::F32,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec4),
                    min: None,
                    max: None,
                    name: Some(format!("WEIGHTS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(weights.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&weights);

                primitives.attributes.insert(
                    validation::Checked::Valid(gltf::Semantic::Joints(0)),
                    Index::new(root.accessors.len() as u32),
                );

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(joints.len() as u64 / 4),
                    component_type: validation::Checked::Valid(GenericComponentType(
                        accessor::ComponentType::U8,
                    )),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec4),
                    min: None,
                    max: None,
                    name: Some(format!("JOINTS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(joints.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&joints);

                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }
            } else {
                panic!("SkeletalMesh must have weights and joints")
            }

            root.scenes[0]
                .nodes
                .push(Index::new(root.nodes.len() as u32));

            root.nodes.push(Node {
                camera: None,
                children: None,
                extensions: None,
                extras: Extras::default(),
                matrix: None,
                mesh: Some(Index::new(root.meshes.len() as u32)),
                name: smesh.name.clone(),
                rotation: None,
                scale: None,
                translation: None,
                skin: Some(Index::new(root.meshes.len() as u32)),
                weights: None,
            });

            root.meshes.push(Mesh {
                extensions: None,
                extras: Extras::default(),
                name: smesh.name.clone(),
                primitives: vec![primitives],
                weights: None,
            });
        }

        let mut textures = HashSet::new();

        for mat in &self.materials {
            if let Some(diffuse) = &mat.diffuse {
                textures.insert(diffuse);
            }

            if let Some(normal) = &mat.normal {
                textures.insert(normal);
            }

            if let Some(glow) = &mat.glow {
                textures.insert(glow);
            }
        }

        for texture in textures {
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
                uri: Some(texture.clone()),
                extensions: None,
                extras: Extras::default(),
            });
        }

        for mat in &self.materials {
            let diffuse_texture = if mat.diffuse.is_some() {
                Some(Info {
                    index: Index::new(
                        root.images
                            .iter()
                            .position(|x| {
                                x.uri
                                    .as_ref()
                                    .is_some_and(|s| s == mat.diffuse.as_ref().unwrap())
                            })
                            .unwrap() as u32,
                    ),
                    tex_coord: 0,
                    extensions: None,
                    extras: Extras::default(),
                })
            } else {
                None
            };

            let normal_texture = if mat.normal.is_some() {
                Some(NormalTexture {
                    index: Index::new(
                        root.images
                            .iter()
                            .position(|x| {
                                x.uri
                                    .as_ref()
                                    .is_some_and(|s| s == mat.normal.as_ref().unwrap())
                            })
                            .unwrap() as u32,
                    ),
                    scale: 1.0,
                    tex_coord: 0,
                    extensions: None,
                    extras: Extras::default(),
                })
            } else {
                None
            };

            let emissive_texture = if mat.glow.is_some() {
                Some(Info {
                    index: Index::new(
                        root.images
                            .iter()
                            .position(|x| {
                                x.uri
                                    .as_ref()
                                    .is_some_and(|s| s == mat.glow.as_ref().unwrap())
                            })
                            .unwrap() as u32,
                    ),
                    tex_coord: 0,
                    extensions: None,
                    extras: Extras::default(),
                })
            } else {
                None
            };

            root.materials.push(Material {
                alpha_cutoff: None,
                alpha_mode: validation::Checked::Valid(material::AlphaMode::Opaque),
                double_sided: false,
                name: None,
                pbr_metallic_roughness: PbrMetallicRoughness {
                    base_color_factor: PbrBaseColorFactor([1.0, 1.0, 1.0, 1.0]),
                    base_color_texture: None,
                    metallic_factor: StrengthFactor(1.0),
                    roughness_factor: StrengthFactor(1.0),
                    metallic_roughness_texture: None,
                    extensions: None,
                    extras: Extras::default(),
                },
                normal_texture,
                occlusion_texture: None,
                emissive_texture,
                emissive_factor: EmissiveFactor([0.0, 0.0, 0.0]),
                extensions: Some(gltf::json::extensions::material::Material {
                    pbr_specular_glossiness: Some(
                        gltf::json::extensions::material::PbrSpecularGlossiness {
                            diffuse_factor: PbrDiffuseFactor([1.0, 1.0, 1.0, 1.0]),
                            diffuse_texture,
                            specular_factor: PbrSpecularFactor([1.0, 1.0, 1.0]),
                            glossiness_factor: StrengthFactor(1.0),
                            specular_glossiness_texture: None,
                            extras: Extras::default(),
                            others: serde_json::map::Map::new(),
                        },
                    ),
                    others: serde_json::map::Map::new(),
                    ..Default::default()
                }),
                extras: Extras::default(),
            });
        }

        if let Some(rotation) = self.scene_root_rotation {
            if !root.scenes[0].nodes.is_empty() {
                let children = std::mem::take(&mut root.scenes[0].nodes);
                let root_index = Index::new(root.nodes.len() as u32);
                root.nodes.push(Node {
                    camera: None,
                    children: Some(children),
                    extensions: None,
                    extras: Extras::default(),
                    matrix: None,
                    mesh: None,
                    name: Some("Creation-to-glTF basis".to_owned()),
                    rotation: Some(UnitQuaternion(rotation)),
                    scale: None,
                    translation: None,
                    skin: None,
                    weights: None,
                });
                root.scenes[0].nodes.push(root_index);
            }
        }

        root.buffers[0].byte_length = USize64(bin_data.len() as u64);

        Ok((root, bin_data))
    }

    pub fn to_glb(&self, name: String) -> Result<Vec<u8>, String> {
        let (mut root, bin_data) = self.to_gltf(name)?;

        root.buffers[0].uri = None;

        let mut json = root
            .to_string()
            .unwrap_or_else(|_| "{}".to_owned())
            .into_bytes();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let mut bin = bin_data;
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let total_len = 12usize
            .saturating_add(8)
            .saturating_add(json.len())
            .saturating_add(8)
            .saturating_add(bin.len());
        let mut glb = Vec::with_capacity(total_len);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_len as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin);
        Ok(glb)
    }
}

impl Default for Model {
    fn default() -> Self {
        Self {
            name: None,
            static_meshes: Vec::new(),
            static_nodes: Vec::new(),
            skeletal_meshes: Vec::new(),
            materials: Vec::new(),
            material_indices: Vec::new(),
            scene_root_rotation: None,
        }
    }
}

fn validate_static_scene(nodes: &[StaticSceneNode], mesh_count: usize) -> Result<(), String> {
    let mut by_block = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        node.validate()?;
        if node.mesh.is_some_and(|mesh| mesh >= mesh_count) {
            return Err(format!(
                "static scene block {} references missing mesh {:?}",
                node.block_index, node.mesh
            ));
        }
        if by_block.insert(node.block_index, index).is_some() {
            return Err(format!("duplicate static scene block {}", node.block_index));
        }
    }

    let mut parents = BTreeMap::new();
    for node in nodes {
        for child in &node.children {
            if !by_block.contains_key(child) {
                return Err(format!(
                    "static scene block {} references missing child {child}",
                    node.block_index
                ));
            }
            if let Some(previous) = parents.insert(*child, node.block_index) {
                return Err(format!(
                    "static scene block {child} has multiple parents {previous} and {}",
                    node.block_index
                ));
            }
        }
    }

    fn visit(
        block: u32,
        nodes: &[StaticSceneNode],
        by_block: &BTreeMap<u32, usize>,
        visiting: &mut HashSet<u32>,
        visited: &mut HashSet<u32>,
    ) -> Result<(), String> {
        if visited.contains(&block) {
            return Ok(());
        }
        if !visiting.insert(block) {
            return Err(format!("cycle detected at static scene block {block}"));
        }
        for child in &nodes[*by_block.get(&block).unwrap()].children {
            visit(*child, nodes, by_block, visiting, visited)?;
        }
        visiting.remove(&block);
        visited.insert(block);
        Ok(())
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for block in by_block.keys().copied() {
        visit(block, nodes, &by_block, &mut visiting, &mut visited)?;
    }
    Ok(())
}

#[cfg(test)]
mod static_scene_tests {
    use super::*;

    fn node(block_index: u32, children: Vec<u32>) -> StaticSceneNode {
        StaticSceneNode {
            block_index,
            name: None,
            translation: glam::Vec3::ZERO,
            rotation: glam::Mat3::IDENTITY,
            scale: 1.0,
            children,
            mesh: None,
        }
    }

    #[test]
    fn accepts_ordered_parent_child_hierarchy() {
        validate_static_scene(
            &[node(10, vec![20]), node(20, vec![30]), node(30, vec![])],
            0,
        )
        .unwrap();
    }

    #[test]
    fn rejects_cycles() {
        let error =
            validate_static_scene(&[node(10, vec![20]), node(20, vec![10])], 0).unwrap_err();
        assert!(error.contains("cycle detected"));
    }

    #[test]
    fn rejects_multiple_parents() {
        let error = validate_static_scene(
            &[node(10, vec![30]), node(20, vec![30]), node(30, vec![])],
            0,
        )
        .unwrap_err();
        assert!(error.contains("multiple parents"));
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Material {
    pub diffuse: Option<String>,
    pub normal: Option<String>,
    pub glow: Option<String>,
    pub specular: Option<String>,
}

/*#[derive(Debug)]
pub struct Model {
    meshes: Vec<Mesh>,
    bones: BoneTree,
    skins: Vec<Skin>,
    imported_skeleton: Option<BoneTree>,
    combined_skeleton: Option<BoneTree>,
    skeletons: Vec<BoneTree>,
    materials: Vec<Material>
}

impl Default for Model {
    fn default() -> Self {
        Model {
            meshes: Vec::new(),
            bones: BoneTree::new(),
            skins: Vec::new(),
            imported_skeleton: None,
            combined_skeleton: None,
            skeletons: Vec::new(),
            materials: Vec::new()
        }
    }
}

impl Model {

    pub fn add_mesh(&mut self, mesh: Mesh) { self.meshes.push(mesh); }
    pub fn add_bone(&mut self, name: &str, bone_data: BoneData) -> Result<(), String> { self.bones.add_bone(name, bone_data) }
    pub fn add_skin(&mut self, skin: Skin) { self.skins.push(skin); }
    pub fn add_material(&mut self, material: Material) { self.materials.push(material); }
    pub fn add_skeleton(&mut self, skeleton: BoneTree) { self.skeletons.push(skeleton); }

    pub fn get_meshes(&self) -> &Vec<Mesh> { &self.meshes }
    pub fn get_bones(&self) -> &BoneTree { &self.bones }
    pub fn get_skins(&self) -> &Vec<Skin> { &self.skins }
    pub fn get_materials(&self) -> &Vec<Material> { &self.materials }
    pub fn get_skeletons(&self) -> &Vec<BoneTree> { &self.skeletons }


    pub fn import_skeleton(&mut self, skeleton: BoneTree) {
        self.imported_skeleton = Some(skeleton.clone());
        self.calculate_combined_skeleton();
    }

    pub fn get_combined_skeleton(&self) -> Option<&BoneTree> {
        self.combined_skeleton.as_ref()
    }

    fn calculate_combined_skeleton(&mut self) {
        if let Some(mut skel) = self.imported_skeleton.clone() {


            for (name, bone_data) in &self.bones.bones {
                if !skel.has_bone(name) {
                    skel.add_bone(name, bone_data.clone()).unwrap();
                }
            }

            self.combined_skeleton = Some(skel);

        } else {
            panic!("Cannot combine skeletons without an imported skeleton. Use import_skeleton() first.")
        }

    }



    pub fn to_gltf(&self, file_name: String) -> (gltf::json::Root, Vec<u8>) {
        use gltf::json::*;
        let mut root = gltf::json::Root::default();
        root.scene = Some(Index::new(0));
        root.scenes.push(Scene {
            extensions: None,
            extras: Extras::default(),
            name: None,
            nodes: Vec::new(),
        });
        let mut bin_data = Vec::new();


        if self.combined_skeleton.is_some() {

            for (name, bone_data) in &self.combined_skeleton.as_ref().unwrap().bones {

                let children = if !bone_data.children.is_empty() {
                    let mut children = Vec::new();
                    for child in &bone_data.children {
                        if let Some(name) = self.combined_skeleton.as_ref().unwrap().bones.iter().position(|x| x.0 == child) {
                            children.push(Index::new(name as u32));
                        } else {
                            panic!("Child bone {} not found", child);
                        }

                    }
                    Some(children)
                } else {
                    None
                };

                let translation = if bone_data.transform.translation.is_some() {
                    let t = bone_data.transform.translation.unwrap();
                    Some([t.x, t.y, t.z])
                } else {
                    None
                };

                let rotation = if bone_data.transform.rotation.is_some() {
                    let r = bone_data.transform.rotation.unwrap();
                    let r = r.to_col_major();
                    let q = Quaternion::from(r);
                    Some(q.into())

                } else {
                    None
                };

                root.nodes.push(Node {
                    camera: None,
                    children,
                    extensions: None,
                    extras: Extras::default(),
                    matrix: None,
                    mesh: None,
                    name: Some(name.to_string()),
                    rotation,
                    scale: None,
                    translation,
                    skin: None,
                    weights: None,
                })


            }

        }

        if let Some(mesh) = self.meshes.iter().max_by_key(|x| x.vertex_positions().len()) {

        //for mesh in &self.meshes {

            let buffer = Index::new(0);


            let mut attributes = BTreeMap::new();






            let positions = mesh.vertex_positions_as_bytes();
            let (min, max) = mesh.get_positions_min_max();
            let normals = mesh.vertex_normals_as_bytes();
            let uvs = mesh.vertex_uvs_as_bytes();
            let triangles = mesh.triangles_as_bytes();
            let weights = mesh.vertex_weights_as_bytes();
            let joints = mesh.vertex_joints_as_bytes();

            if positions.len() > 0 {
                attributes.insert(validation::Checked::Valid(gltf::Semantic::Positions), Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(positions.len() as u64 / 12),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec3),
                    min: Some(Value::Array(vec![min.x.into(), min.y.into(), min.z.into()])),
                    max: Some(Value::Array(vec![max.x.into(), max.y.into(), max.z.into()])),
                    name: Some(format!("POSITIONS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer,
                    byte_length: USize64(positions.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&positions);

            }

            if normals.len() > 0 {
                attributes.insert(validation::Checked::Valid(gltf::Semantic::Normals), Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(normals.len() as u64 / 12),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec3),
                    min: None,
                    max: None,
                    name: Some(format!("NORMALS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer,
                    byte_length: USize64(normals.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&normals);

            }

            if uvs.len() > 0 {
                attributes.insert(validation::Checked::Valid(gltf::Semantic::TexCoords(0)), Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(uvs.len() as u64 / 8),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec2),
                    min: None,
                    max: None,
                    name: Some(format!("UVS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer,
                    byte_length: USize64(uvs.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&uvs);

            }

            if weights.len() > 0 {
                attributes.insert(validation::Checked::Valid(gltf::Semantic::Weights(0)), Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(weights.len() as u64 / 16),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec4),
                    min: None,
                    max: None,
                    name: Some(format!("WEIGHTS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer,
                    byte_length: USize64(weights.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&weights);

            }

            if joints.len() > 0 {
                attributes.insert(validation::Checked::Valid(gltf::Semantic::Joints(0)), Index::new(root.accessors.len() as u32));
                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(joints.len() as u64 / 4),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::U8)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec4),
                    min: None,
                    max: None,
                    name: Some(format!("JOINTS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer,
                    byte_length: USize64(joints.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });

                bin_data.extend_from_slice(&joints);

            }

            if triangles.len() > 0 {

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(triangles.len() as u64 / 2),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::U16)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Scalar),
                    min: None,
                    max: None,
                    name: Some(format!("TRIANGLES | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer,
                    byte_length: USize64(triangles.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    extensions: None,
                    extras: Extras::default(),
                    name: None,
                    target: None,
                });


                bin_data.extend_from_slice(&triangles);


                if bin_data.len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }

            }

            let mut primitives = Vec::new();
            primitives.push(Primitive {
                attributes,
                extensions: None,
                extras: None,
                indices: Some(Index::new(root.accessors.len() as u32 - 1)),
                material: None,
                mode: validation::Checked::Valid(mesh::Mode::Triangles),
                targets: None,
            });

            root.meshes.push(Mesh {
                extensions: None,
                extras: Extras::default(),
                name: Some(mesh.name.clone()),
                primitives,
                weights: None,
            });

            let skin = if self.skins.len() > 0 {
                Some(Index::new(root.meshes.len() as u32 - 1))
            } else {
                None
            };


            root.nodes.push(Node {
                camera: None,
                children: None,
                extensions: None,
                extras: None,
                matrix: None,
                mesh: Some(Index::new(root.meshes.len() as u32 - 1)),
                name: Some(mesh.name.clone()),
                rotation: None,
                scale: None,
                translation: None,
                skin,
                weights: None,
            });

        }


        // Test code
        if let Some(root_pos) = root.nodes.iter().position(|x| x.name.as_ref().is_some_and(|s| s.starts_with("Root"))) {
        /*let export_pos = root.nodes.iter().position(|x| x.name.as_ref().is_some_and(|s| s.starts_with("SkeletonExport"))).unwrap();
        let com_pos = root.nodes.iter().position(|x| x.name.as_ref().is_some_and(|s| s.starts_with("COM"))).unwrap();*/

        let skin = self.skins.iter().max_by_key(|x| x.joints().len()).unwrap();

        //for skin in &self.skins {

            let mut joints = Vec::new();

            for skin_joint in skin.joints() {
                let index = self.combined_skeleton.as_ref().unwrap().bones.iter().position(|x| x.0 == skin_joint).unwrap();
                joints.push(Index::new(index as u32));

            }

            let mut inverse_bytes = Vec::new();

            for i in skin.inverse_binds() {
                inverse_bytes.extend_from_slice(&i.as_bytes())
            }

            root.accessors.push(Accessor {
                buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                byte_offset: None,
                count: USize64(skin.inverse_binds().len() as u64),
                component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                extensions: None,
                extras: Extras::default(),
                type_: validation::Checked::Valid(accessor::Type::Mat4),
                min: None,
                max: None,
                name: Some(format!("INVERSE_BINDS | SKIN:{}", root.skins.len())),
                normalized: false,
                sparse: None,
            });

            root.buffer_views.push(View {
                buffer: Index::new(0),
                byte_length: USize64(inverse_bytes.len() as u64),
                byte_offset: Some(USize64(bin_data.len() as u64)),
                byte_stride: None,
                extensions: None,
                extras: Extras::default(),
                name: None,
                target: None,
            });

            bin_data.extend_from_slice(&inverse_bytes);

            root.skins.push(Skin {
                extensions: None,
                extras: Extras::default(),
                inverse_bind_matrices: Some(Index::new(root.accessors.len() as u32 - 1)),
                joints,
                name: None,
                skeleton: None,
            });


        //}
            root.scenes[0].nodes.push(Index::new(root_pos as u32));
        }

        if let Some(base) = root.nodes.iter_mut().find(|x| x.name.as_ref().is_some_and(|s| s.starts_with("BASE"))) {
            base.children = None;
        }

        if let Some(base) = root.nodes.iter_mut().find(|x| x.name.as_ref().is_some_and(|s| s.starts_with("SkeletonExport"))) {
            base.scale = Some([0.01, 0.01, 0.01]);
        }

        root.buffers.push(Buffer {
            byte_length: USize64(bin_data.len() as u64),
            name: None,
            uri: Some(format!("{}.bin", file_name)),
            extensions: None,
            extras: Extras::default(),
        });




        (root, bin_data)

    }

    pub fn to_gltf_v2(&self, model_name: String) -> (gltf::json::Root, Vec<u8>) {

        use gltf::json::*;

        let mut root = gltf::json::Root::default();
        let mut bin_data = Vec::new();

        let has_skeleton = if self.skeletons.len() > 0 { true } else { false };
        let mut skeleton_index = 0;


        if has_skeleton {
            for skeleton in &self.skeletons {
                for (name, bone_data) in &skeleton.bones {

                    let translation = if bone_data.transform.translation.is_some() {
                        let t = bone_data.transform.translation.unwrap();
                        if t == Vec3::zero() {
                            None
                        } else {
                            Some([t.x, t.y, t.z])
                        }
                    } else {
                        None
                    };

                    let rotation = if bone_data.transform.rotation.is_some() {
                        let r = bone_data.transform.rotation.unwrap();
                        let r = r.to_col_major();
                        let q = Quaternion::from(r);
                        Some(q.into())

                    } else {
                        None
                    };


                    root.nodes.push(Node {
                        camera: None,
                        children: None,
                        extensions: None,
                        extras: None,
                        matrix: None,
                        mesh: None,
                        name: Some(format!("{}:{}", name, skeleton_index)),
                        rotation,
                        scale: None,
                        translation,
                        skin: None,
                        weights: None,
                    });



                }
                skeleton_index += 1;
            }
        }

        for mesh in &self.meshes {

            let positions = mesh.vertex_positions_as_bytes();
            let (min, max) = mesh.get_positions_min_max();
            let mut attributes = BTreeMap::new();

            attributes.insert(validation::Checked::Valid(gltf::Semantic::Positions), Index::new(root.accessors.len() as u32));

            root.accessors.push(Accessor {
                buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                byte_offset: None,
                count: USize64(positions.len() as u64 / 12),
                component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                extensions: None,
                extras: Extras::default(),
                type_: validation::Checked::Valid(accessor::Type::Vec3),
                min: Some(Value::Array(vec![min.x.into(), min.y.into(), min.z.into()])),
                max: Some(Value::Array(vec![max.x.into(), max.y.into(), max.z.into()])),
                name: Some(format!("POSITIONS | MESH:{}", root.meshes.len())),
                normalized: false,
                sparse: None,
            });

            root.buffer_views.push(View {
                buffer: Index::new(0),
                byte_length: USize64(positions.len() as u64),
                byte_offset: Some(USize64(bin_data.len() as u64)),
                byte_stride: None,
                name: Some(format!("POSITIONS | MESH:{}", root.meshes.len())),
                target: None,
                extensions: None,
                extras: Extras::default(),
            });

            bin_data.extend_from_slice(&positions);

            let normals = mesh.vertex_normals_as_bytes();

            if normals.len() > 0 {

                attributes.insert(validation::Checked::Valid(gltf::Semantic::Normals), Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(positions.len() as u64 / 12),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec3),
                    min: None,
                    max: None,
                    name: Some(format!("NORMALS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });


                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(normals.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    name: Some(format!("NORMALS | MESH:{}", root.meshes.len())),
                    target: None,
                    extensions: None,
                    extras: Extras::default(),
                });

                bin_data.extend_from_slice(&normals);

            }

            let triangles = mesh.triangles_as_bytes();
            let mut indices = None;

            if triangles.len() > 0 {

                indices = Some(Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(triangles.len() as u64 / 2),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::U16)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Scalar),
                    min: None,
                    max: None,
                    name: Some(format!("TRIANGLES | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(triangles.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    name: Some(format!("TRIANGLES | MESH:{}", root.meshes.len())),
                    target: None,
                    extensions: None,
                    extras: Extras::default(),
                });

                bin_data.extend_from_slice(&triangles);

                if bin_data .len() % 4 != 0 {
                    let padding = 4 - (bin_data.len() % 4);
                    bin_data.extend_from_slice(&vec![0; padding]);
                }

            }

            let uvs = mesh.vertex_uvs_as_bytes();

            if uvs.len() > 0 {

                attributes.insert(validation::Checked::Valid(gltf::Semantic::TexCoords(0)), Index::new(root.accessors.len() as u32));

                root.accessors.push(Accessor {
                    buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                    byte_offset: None,
                    count: USize64(uvs.len() as u64 / 8),
                    component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                    extensions: None,
                    extras: Extras::default(),
                    type_: validation::Checked::Valid(accessor::Type::Vec2),
                    min: None,
                    max: None,
                    name: Some(format!("UVS | MESH:{}", root.meshes.len())),
                    normalized: false,
                    sparse: None,
                });

                root.buffer_views.push(View {
                    buffer: Index::new(0),
                    byte_length: USize64(uvs.len() as u64),
                    byte_offset: Some(USize64(bin_data.len() as u64)),
                    byte_stride: None,
                    name: Some(format!("UVS | MESH:{}", root.meshes.len())),
                    target: None,
                    extensions: None,
                    extras: Extras::default(),
                });

                bin_data.extend_from_slice(&uvs);

            }

            if has_skeleton {

                let weights = mesh.vertex_weights_as_bytes();

                if weights.len() > 0 {

                    attributes.insert(validation::Checked::Valid(gltf::Semantic::Weights(0)), Index::new(root.accessors.len() as u32));

                    root.accessors.push(Accessor {
                        buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                        byte_offset: None,
                        count: USize64(weights.len() as u64 / 16),
                        component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::F32)),
                        extensions: None,
                        extras: Extras::default(),
                        type_: validation::Checked::Valid(accessor::Type::Vec4),
                        min: None,
                        max: None,
                        name: Some(format!("WEIGHTS | MESH:{}", root.meshes.len())),
                        normalized: false,
                        sparse: None,
                    });

                    root.buffer_views.push(View {
                        buffer: Index::new(0),
                        byte_length: USize64(weights.len() as u64),
                        byte_offset: Some(USize64(bin_data.len() as u64)),
                        byte_stride: None,
                        name: Some(format!("WEIGHTS | MESH:{}", root.meshes.len())),
                        target: None,
                        extensions: None,
                        extras: Extras::default(),
                    });

                    bin_data.extend_from_slice(&weights);

                }

                let joints = mesh.vertex_joints_as_bytes();

                if joints.len() > 0 {

                    attributes.insert(validation::Checked::Valid(gltf::Semantic::Joints(0)), Index::new(root.accessors.len() as u32));

                    root.accessors.push(Accessor {
                        buffer_view: Some(Index::new(root.buffer_views.len() as u32)),
                        byte_offset: None,
                        count: USize64(joints.len() as u64 / 4),
                        component_type: validation::Checked::Valid(GenericComponentType(accessor::ComponentType::U8)),
                        extensions: None,
                        extras: Extras::default(),
                        type_: validation::Checked::Valid(accessor::Type::Vec4),
                        min: None,
                        max: None,
                        name: Some(format!("JOINTS | MESH:{}", root.meshes.len())),
                        normalized: false,
                        sparse: None,
                    });

                    root.buffer_views.push(View {
                        buffer: Index::new(0),
                        byte_length: USize64(joints.len() as u64),
                        byte_offset: Some(USize64(bin_data.len() as u64)),
                        byte_stride: None,
                        name: Some(format!("JOINTS | MESH:{}", root.meshes.len())),
                        target: None,
                        extensions: None,
                        extras: Extras::default(),
                    });

                    bin_data.extend_from_slice(&joints);

                }



            }


            root.meshes.push(Mesh {
                extensions: None,
                extras: Extras::default(),
                name: Some(mesh.name.clone()),
                primitives: vec![Primitive {
                    attributes,
                    extensions: None,
                    extras: Extras::default(),
                    indices,
                    material: None,
                    mode: validation::Checked::Valid(mesh::Mode::Triangles),
                    targets: None,
                }],
                weights: None,
            });

        }


        root.buffers.push(Buffer {
            byte_length: USize64(bin_data.len() as u64),
            name: None,
            uri: Some(format!("{}.bin", model_name)),
            extensions: None,
            extras: Extras::default(),
        });


        (root, bin_data)
    }

}




#[derive(Debug)]
pub struct SkeletalMesh {
    pub name: Option<String>,
    pub meshes: Vec<Mesh>,
    pub bones: HashMap<String, Vec<String>>,
    pub materials: HashMap<usize, Material>,
    pub skins: HashMap<usize, Skin>
}

impl SkeletalMesh {

    pub fn add_mesh(&mut self, mesh: Mesh) { self.meshes.push(mesh); }
    pub fn add_bone(&mut self, name: &str, bone_data: Vec<String>) { self.bones.insert(name.to_string(), bone_data); }
    pub fn add_material(&mut self, mesh: usize, material: Material) { self.materials.insert(mesh, material); }

    pub fn add_child_to_bone(&mut self, parent: &str, child: &str) -> Result<(), String> {
        if let Some(bone) = self.bones.get_mut(parent) {
            bone.push(child.to_string());
            Ok(())
        } else {
            error!("add_child_to_bone: Bone {} not found, cannot add child.", parent);
            Err(format!("Bone {} not found", parent))
        }
    }

    pub fn import_bones(&mut self, bones: BoneTree) {
        for (name, bone_data) in &bones.bones {
            let mut children = Vec::new();
            for child in &bone_data.children {
                children.push(child.to_string());
            }
            self.bones.insert(name.to_string(), children);
        }
    }

}

impl Default for SkeletalMesh {
    fn default() -> Self {
        SkeletalMesh {
            name: None,
            meshes: Vec::new(),
            bones: HashMap::new(),
            materials: HashMap::new(),
            skins: HashMap::new()
        }
    }
}*/
