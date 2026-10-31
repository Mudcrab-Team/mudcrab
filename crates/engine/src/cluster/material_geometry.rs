//! Vertex inputs required by the actual StandardMaterial, independent of optional PBR features.
//!
//! MCL currently stores UV0 only. Bevy 0.19's pbr_fragment.wgsl consumes TBN for
//! normal/clearcoat-normal maps, parallax depth maps and nonzero anisotropy.

use bevy::{
    asset::Handle,
    image::Image,
    mesh::UvChannel,
    pbr::StandardMaterial,
    reflect::{PartialReflect, ReflectRef, structs::Struct},
};

pub(super) use shared::cluster_material::GeometryRequirements;
use shared::cluster_material::{GeometryField, geometry_requirements};

pub(super) fn requirements(material: &StandardMaterial) -> GeometryRequirements {
    let ReflectRef::Struct(fields) = material.reflect_ref() else {
        return GeometryRequirements {
            supported: false,
            tangents: false,
        };
    };
    projected_requirements(fields)
}

fn projected_requirements(fields: &dyn Struct) -> GeometryRequirements {
    geometry_requirements((0..fields.field_len()).flat_map(|index| {
        let mut projected = [None; 3];
        let (Some(name), Some(value)) = (fields.name_at(index), fields.field_at(index)) else {
            projected[0] = Some(GeometryField::Unknown);
            return projected.into_iter().flatten();
        };
        if let Some(channel) = value.try_downcast_ref::<UvChannel>() {
            let texture = name
                .strip_suffix("_channel")
                .and_then(|prefix| fields.field(&format!("{prefix}_texture")))
                .and_then(|field| field.try_downcast_ref::<Option<Handle<Image>>>());
            projected[0] = Some(GeometryField::Channel {
                uv0: *channel == UvChannel::Uv0,
                texture_present: texture.map(Option::is_some),
            });
        } else if name.ends_with("_channel") {
            projected[0] = Some(GeometryField::UnknownChannel);
        }
        if let Some(texture) = value.try_downcast_ref::<Option<Handle<Image>>>()
            && texture.is_some()
        {
            projected[1] = Some(GeometryField::PresentTexture(name));
        }
        if name == "anisotropy_strength" {
            projected[2] = Some(GeometryField::Anisotropy(
                value.try_downcast_ref::<f32>().copied(),
            ));
        }
        projected.into_iter().flatten()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::reflect::{ReflectMut, structs::DynamicStruct};

    #[test]
    fn default_and_unused_uv1_do_not_require_extra_vertex_inputs() {
        let mut material = StandardMaterial::default();
        assert_eq!(
            requirements(&material),
            GeometryRequirements {
                supported: true,
                tangents: false
            }
        );
        material.base_color_channel = UvChannel::Uv1;
        assert_eq!(
            requirements(&material),
            GeometryRequirements {
                supported: true,
                tangents: false
            }
        );
        material.base_color_texture = Some(Handle::default());
        assert!(!requirements(&material).supported);
        material.base_color_channel = UvChannel::Uv0;
        assert_eq!(
            requirements(&material),
            GeometryRequirements {
                supported: true,
                tangents: false
            }
        );
    }

    #[test]
    fn normal_parallax_and_anisotropy_each_require_tangents() {
        let normal = StandardMaterial {
            normal_map_texture: Some(Handle::default()),
            ..Default::default()
        };
        let parallax = StandardMaterial {
            depth_map: Some(Handle::default()),
            ..Default::default()
        };
        let anisotropic = StandardMaterial {
            anisotropy_strength: 0.5,
            ..Default::default()
        };
        for material in [normal, parallax, anisotropic] {
            assert_eq!(
                requirements(&material),
                GeometryRequirements {
                    supported: true,
                    tangents: true
                }
            );
        }
    }

    #[test]
    fn every_present_optional_channel_checks_its_actual_texture() {
        let mut material = StandardMaterial::default();
        let names: Vec<String> = match material.reflect_ref() {
            ReflectRef::Struct(fields) => (0..fields.field_len())
                .filter_map(|index| {
                    let name = fields.name_at(index)?;
                    fields.field_at(index)?.try_downcast_ref::<UvChannel>()?;
                    Some(name.to_owned())
                })
                .collect(),
            _ => panic!("StandardMaterial must reflect as a struct"),
        };
        for name in names {
            let texture_name = format!("{}_texture", name.strip_suffix("_channel").unwrap());
            let ReflectMut::Struct(fields) = material.reflect_mut() else {
                unreachable!()
            };
            *fields
                .field_mut(&name)
                .unwrap()
                .try_downcast_mut::<UvChannel>()
                .unwrap() = UvChannel::Uv1;
            assert!(requirements(&material).supported, "unused {name}");
            let ReflectMut::Struct(fields) = material.reflect_mut() else {
                unreachable!()
            };
            *fields
                .field_mut(&texture_name)
                .unwrap()
                .try_downcast_mut::<Option<Handle<Image>>>()
                .unwrap() = Some(Handle::default());
            assert!(!requirements(&material).supported, "textured {name}");
            material = StandardMaterial::default();
        }
    }

    #[test]
    fn optional_clearcoat_normal_requires_tangents_when_present() {
        let mut material = StandardMaterial::default();
        let ReflectMut::Struct(fields) = material.reflect_mut() else {
            unreachable!()
        };
        let Some(texture) = fields.field_mut("clearcoat_normal_texture") else {
            return;
        };
        *texture.try_downcast_mut::<Option<Handle<Image>>>().unwrap() = Some(Handle::default());
        assert_eq!(
            requirements(&material),
            GeometryRequirements {
                supported: true,
                tangents: true
            }
        );
    }

    #[test]
    fn unknown_channel_or_unnamed_texture_cannot_certify_compatibility() {
        let mut fields = DynamicStruct::default();
        fields.insert("future_channel", UvChannel::Uv1);
        assert!(!projected_requirements(&fields).supported);
        let mut fields = DynamicStruct::default();
        fields.insert("future_image", Some(Handle::<Image>::default()));
        assert!(!projected_requirements(&fields).supported);
    }
}

#[cfg(test)]
mod tangent_requirement_regressions {
    use super::*;
    use bevy::reflect::structs::DynamicStruct;

    #[test]
    fn all_tangent_consumers_are_classified_even_without_optional_cargo_features() {
        for name in [
            "normal_map_texture",
            "clearcoat_normal_texture",
            "depth_map",
        ] {
            let mut fields = DynamicStruct::default();
            fields.insert(name, Some(Handle::<Image>::default()));
            assert_eq!(
                projected_requirements(&fields),
                GeometryRequirements {
                    supported: true,
                    tangents: true
                },
                "{name}"
            );
        }
        let mut fields = DynamicStruct::default();
        fields.insert("anisotropy_strength", 0.5_f32);
        assert!(projected_requirements(&fields).tangents);
        let mut fields = DynamicStruct::default();
        fields.insert("anisotropy_strength", 0.0_f32);
        assert!(!projected_requirements(&fields).tangents);
    }
}
