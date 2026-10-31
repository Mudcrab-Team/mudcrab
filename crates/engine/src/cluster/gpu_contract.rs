//! Host/GPU contract shared by renderer uploads and hardware integration tests.

/// Execution order and explicit bindings of the production compaction pipelines.
/// The scatter layouts use exactly eight storage buffers, the portable limit checked
/// by gpu_support. Do not add another without splitting the stage or raising that check.
pub(crate) const STAGES: [(&str, &[u32]); 8] = [
    ("cull_instances", &[0, 3, 6, 8, 10, 11, 13]),
    ("prefix_instances", &[0, 9, 11]),
    ("scatter_instances", &[0, 8, 11]),
    ("cull", &[0, 1, 2, 3, 6, 8, 11, 12]),
    ("prefix_draw_tiles", &[0, 8, 11, 13]),
    ("prefix_draw_groups", &[5, 11, 13]),
    ("scatter_clusters", &[0, 3, 4, 6, 7, 8, 11, 12, 13]),
    ("scatter_large_clusters", &[0, 3, 4, 6, 7, 8, 11, 12, 13]),
];

/// Wgpu's five-word indexed indirect ABI, shared by allocations and draw slice offsets.
pub(crate) const DRAW_INDEXED_INDIRECT_BYTES: u64 =
    std::mem::size_of::<wgpu::util::DrawIndexedIndirectArgs>() as u64;

/// Selection and first_index carried from cull to scatter without another storage binding.
pub(crate) const SELECTED_CLUSTER_BYTES: u64 = 8;

/// Exact 128-byte WGSL CullParams uniform, without host-only padding or offsets.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CullParams {
    pub planes: [[f32; 4]; 5],
    pub camera: [f32; 4],
    pub error_pixels: f32,
    pub near: f32,
    /// Reserved alignment word, always zero. Instance size never controls visibility.
    pub reserved_instance_padding: f32,
    pub instance_count: u32,
    pub orthographic: u32,
    pub terrain_error_pixels: f32,
    pub instance_mask_base: u32,
    pub epoch: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn indexed_selection_keeps_the_portable_scatter_binding_limit() {
        let module = naga_oil::compose::Composer::default()
            .make_naga_module(naga_oil::compose::NagaModuleDescriptor {
                source: include_str!("shaders/cluster_cull.wgsl"),
                file_path: "cluster_cull.wgsl",
                ..Default::default()
            })
            .expect("production shader validates");
        let (_, scratch) = module
            .global_variables
            .iter()
            .find(|(_, global)| global.name.as_deref() == Some("selected_clusters"))
            .expect("selection scratch");
        let wgpu::naga::TypeInner::Array { base, stride, .. } = module.types[scratch.ty].inner
        else {
            panic!("selection scratch must remain an array");
        };
        assert_eq!(u64::from(stride), SELECTED_CLUSTER_BYTES);
        assert_eq!(
            module.types[base].inner,
            wgpu::naga::TypeInner::Vector {
                size: wgpu::naga::VectorSize::Bi,
                scalar: wgpu::naga::Scalar::U32
            }
        );
        for (entry, bindings) in STAGES {
            let storage = module
                .global_variables
                .iter()
                .filter(|(_, global)| {
                    matches!(global.space, wgpu::naga::AddressSpace::Storage { .. })
                        && global
                            .binding
                            .as_ref()
                            .is_some_and(|binding| bindings.contains(&binding.binding))
                })
                .count();
            assert!(storage <= 8, "{entry} needs {storage} storage bindings");
        }
    }

    #[test]
    fn indexed_indirect_args_match_wgpu() {
        use wgpu::{naga::TypeInner, util::DrawIndexedIndirectArgs};

        let module = naga_oil::compose::Composer::default()
            .make_naga_module(naga_oil::compose::NagaModuleDescriptor {
                source: include_str!("shaders/cluster_cull.wgsl"),
                file_path: "cluster_cull.wgsl",
                ..Default::default()
            })
            .expect("production shader validates");
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("DrawArgs"))
            .expect("draw argument structure");
        let TypeInner::Struct { members, span } = &ty.inner else {
            panic!("draw arguments must be a structure");
        };
        let expected = [
            (
                "index_count",
                offset_of!(DrawIndexedIndirectArgs, index_count),
            ),
            (
                "instance_count",
                offset_of!(DrawIndexedIndirectArgs, instance_count),
            ),
            (
                "first_index",
                offset_of!(DrawIndexedIndirectArgs, first_index),
            ),
            (
                "base_vertex",
                offset_of!(DrawIndexedIndirectArgs, base_vertex),
            ),
            (
                "first_instance",
                offset_of!(DrawIndexedIndirectArgs, first_instance),
            ),
        ];
        assert_eq!(*span as u64, DRAW_INDEXED_INDIRECT_BYTES);
        assert_eq!(*span as usize, size_of::<DrawIndexedIndirectArgs>());
        assert_eq!(members.len(), expected.len());
        for (member, (name, offset)) in members.iter().zip(expected) {
            assert_eq!(member.name.as_deref(), Some(name));
            assert_eq!(member.offset as usize, offset, "{name}");
            let kind = if name == "base_vertex" {
                wgpu::naga::ScalarKind::Sint
            } else {
                wgpu::naga::ScalarKind::Uint
            };
            assert_eq!(
                module.types[member.ty].inner,
                TypeInner::Scalar(wgpu::naga::Scalar { kind, width: 4 })
            );
        }
    }

    #[test]
    fn production_cull_does_not_read_the_host_epoch() {
        use wgpu::naga::{Expression, TypeInner, valid};

        let module = naga_oil::compose::Composer::default()
            .make_naga_module(naga_oil::compose::NagaModuleDescriptor {
                source: include_str!("shaders/cluster_cull.wgsl"),
                file_path: "cluster_cull.wgsl",
                ..Default::default()
            })
            .expect("production shader validates");
        let info = valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::all())
            .validate(&module)
            .expect("production shader has resolved types");
        let (params_type, ty) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("CullParams"))
            .expect("cull uniform type");
        let TypeInner::Struct { members, .. } = &ty.inner else {
            panic!("cull uniform must be a struct");
        };
        let epoch_index = members
            .iter()
            .position(|member| member.name.as_deref() == Some("epoch"))
            .expect("reserved epoch ABI field") as u32;
        let check = |function: &wgpu::naga::Function, info: &valid::FunctionInfo| {
            for (_, expression) in function.expressions.iter() {
                if let Expression::AccessIndex { base, index } = *expression {
                    let base_type = info[base].ty.inner_with(&module.types);
                    let is_params = match *base_type {
                        TypeInner::Pointer { base, .. } => base == params_type,
                        TypeInner::Struct { .. } => info[base].ty.handle() == Some(params_type),
                        _ => false,
                    };
                    assert!(
                        !is_params || index != epoch_index,
                        "the upload cache normalizes epoch; the shader must not read it"
                    );
                }
                if let Expression::Load { pointer } = *expression {
                    let ty = info[pointer].ty.inner_with(&module.types);
                    assert!(
                        !matches!(*ty, TypeInner::Pointer { base, .. } if base == params_type),
                        "read cull members individually so epoch use stays checked"
                    );
                }
            }
        };
        for (handle, function) in module.functions.iter() {
            check(function, &info[handle]);
        }
        for (index, entry) in module.entry_points.iter().enumerate() {
            check(&entry.function, info.get_entry_point(index));
        }
    }

    #[test]
    fn cull_uniform_has_no_instance_size_policy() {
        let module = naga_oil::compose::Composer::default()
            .make_naga_module(naga_oil::compose::NagaModuleDescriptor {
                source: include_str!("shaders/cluster_cull.wgsl"),
                file_path: "cluster_cull.wgsl",
                ..Default::default()
            })
            .expect("production shader validates");
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("CullParams"))
            .expect("CullParams");
        let wgpu::naga::TypeInner::Struct { members, .. } = &ty.inner else {
            panic!("CullParams must remain a struct");
        };
        assert!(
            members
                .iter()
                .all(|member| member.name.as_deref() != Some("min_instance_pixels"))
        );
    }

    #[test]
    fn cull_uniform_matches_production_wgsl_layout() {
        let module = naga_oil::compose::Composer::default()
            .make_naga_module(naga_oil::compose::NagaModuleDescriptor {
                source: include_str!("shaders/cluster_cull.wgsl"),
                file_path: "cluster_cull.wgsl",
                ..Default::default()
            })
            .expect("production shader validates");
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("CullParams"))
            .expect("CullParams struct");
        let wgpu::naga::TypeInner::Struct { members, span } = &ty.inner else {
            panic!("CullParams must remain a struct");
        };
        let expected = [
            ("planes", offset_of!(CullParams, planes)),
            ("camera", offset_of!(CullParams, camera)),
            ("error_pixels", offset_of!(CullParams, error_pixels)),
            ("near", offset_of!(CullParams, near)),
            (
                "reserved_instance_padding",
                offset_of!(CullParams, reserved_instance_padding),
            ),
            ("instance_count", offset_of!(CullParams, instance_count)),
            ("orthographic", offset_of!(CullParams, orthographic)),
            (
                "terrain_error_pixels",
                offset_of!(CullParams, terrain_error_pixels),
            ),
            (
                "instance_mask_base",
                offset_of!(CullParams, instance_mask_base),
            ),
            ("epoch", offset_of!(CullParams, epoch)),
        ];
        assert_eq!(*span as usize, size_of::<CullParams>());
        assert_eq!(members.len(), expected.len());
        for (member, (name, offset)) in members.iter().zip(expected) {
            assert_eq!(member.name.as_deref(), Some(name));
            assert_eq!(member.offset as usize, offset, "{name}");
        }
    }
}
