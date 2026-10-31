//! Material certification shared by cluster publication and runtime adapters.
//!
//! This crate owns admission, vertex requirements and stock material labels. Renderer-specific
//! reflection and asset loading project into this contract; they are not builder source inputs.
use serde_json::Value;

/// Version of the material certification semantics. Changes also invalidate the hashed source.
pub const CERTIFICATION_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryRequirements {
    pub supported: bool,
    pub tangents: bool,
}

/// A renderer-independent projection of one material field.
#[derive(Clone, Copy, Debug)]
pub enum GeometryField<'a> {
    /// A selected UV channel and whether its paired texture is recognized and present.
    Channel {
        uv0: bool,
        texture_present: Option<bool>,
    },
    UnknownChannel,
    PresentTexture(&'a str),
    Anisotropy(Option<f32>),
    Unknown,
}

/// MCL carries UV0. Normal, parallax and anisotropy shading additionally need tangents.
/// Unknown projected inputs cannot establish compatible geometry.
pub fn geometry_requirements<'a>(
    fields: impl IntoIterator<Item = GeometryField<'a>>,
) -> GeometryRequirements {
    let mut result = GeometryRequirements {
        supported: true,
        tangents: false,
    };
    for field in fields {
        match field {
            GeometryField::Channel {
                uv0,
                texture_present,
            } => {
                if !uv0 && texture_present.is_none_or(|present| present) {
                    result.supported = false;
                }
            }
            GeometryField::UnknownChannel | GeometryField::Unknown => result.supported = false,
            GeometryField::PresentTexture(name) => {
                if name != "depth_map" && !name.ends_with("_texture") {
                    result.supported = false;
                }
                if matches!(
                    name,
                    "normal_map_texture" | "clearcoat_normal_texture" | "depth_map"
                ) {
                    result.tangents = true;
                }
            }
            GeometryField::Anisotropy(Some(strength)) => result.tangents |= strength != 0.0,
            GeometryField::Anisotropy(None) => result.supported = false,
        }
    }
    result
}

/// The stock glTF material label used for a published material slot.
/// The renderer tests this format against its actual asset-label implementation.
pub fn material_asset_path(model: &str, slot: u32, inverted: bool, native: bool) -> String {
    format!(
        "{model}#Material{slot}{}{suffix}",
        if inverted { " (inverted)" } else { "" },
        suffix = if native { "/nif" } else { "/std" }
    )
}

/// Refuse shader features outside the certified stock-versus-cluster contract.
pub fn unsupported_feature(material: &Value, family: Option<&str>) -> Option<String> {
    if family != Some("lighting") {
        return Some(format!("shaderFamily {}", family.unwrap_or("none")));
    }
    let lighting_type = material
        .pointer("/extras/openSkyrim/lightingShaderType")
        .and_then(Value::as_str);
    if let Some(kind) = lighting_type.filter(|kind| *kind != "default") {
        return Some(format!("lightingShaderType {kind}"));
    }
    // Only the directx-convention tangent-space normal map is shaded the way the stock path
    // does it; a model-space normal map would shade wrongly once node transforms are baked in.
    if let Some(convention) = material
        .pointer("/extras/openSkyrim/normalConvention")
        .and_then(Value::as_str)
        .filter(|convention| *convention != "directx")
    {
        return Some(format!("normalConvention {convention}"));
    }
    for flag in ["premultipliedAlpha", "screenDoorAlphaFade"] {
        let pointer = format!("/extensions/OPEN_SKYRIM_material/{flag}");
        if material.pointer(&pointer).and_then(Value::as_bool) == Some(true) {
            return Some(flag.to_owned());
        }
    }
    if material.get("emissiveTexture").is_some() {
        return Some("emissive texture".into());
    }
    let emissive = material
        .get("emissiveFactor")
        .and_then(Value::as_array)
        .is_some_and(|factor| factor.iter().any(|v| v.as_f64().is_some_and(|v| v > 0.0)));
    emissive.then(|| "emissive factor".into())
}

pub fn stock_requires_sorted_depth(material: &Value, blended: bool) -> bool {
    let flags = |name: &str| {
        material
            .get("extras")?
            .get("openSkyrim")?
            .get(name)?
            .as_u64()
            .and_then(|flags| u32::try_from(flags).ok())
    };
    flags("shaderFlags1")
        .zip(flags("shaderFlags2"))
        .is_some_and(|(flags_1, flags_2)| {
            crate::material_depth::requires_sorted_depth_pass(flags_1, flags_2, blended)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_uv1_texture_is_allowed_but_present_and_unknown_partners_are_refused() {
        for (present, expected) in [(Some(false), true), (Some(true), false), (None, false)] {
            assert_eq!(
                geometry_requirements([GeometryField::Channel {
                    uv0: false,
                    texture_present: present
                }])
                .supported,
                expected
            );
        }
        assert!(!geometry_requirements([GeometryField::UnknownChannel]).supported);
        assert!(!geometry_requirements([GeometryField::Unknown]).supported);
    }
    #[test]
    fn tangent_consumers_and_unknown_texture_inputs_keep_the_same_contract() {
        for name in [
            "normal_map_texture",
            "clearcoat_normal_texture",
            "depth_map",
        ] {
            assert_eq!(
                geometry_requirements([GeometryField::PresentTexture(name)]),
                GeometryRequirements {
                    supported: true,
                    tangents: true
                }
            );
        }
        assert!(!geometry_requirements([GeometryField::PresentTexture("future_image")]).supported);
        assert!(!geometry_requirements([GeometryField::Anisotropy(Some(0.0))]).tangents);
        assert!(geometry_requirements([GeometryField::Anisotropy(Some(0.5))]).tangents);
        assert!(!geometry_requirements([GeometryField::Anisotropy(None)]).supported);
    }
    #[test]
    fn material_labels_cover_both_winding_and_handler_choices() {
        assert_eq!(
            material_asset_path("meshes/model.glb", 7, false, false),
            "meshes/model.glb#Material7/std"
        );
        assert_eq!(
            material_asset_path("meshes/model.glb", 7, true, true),
            "meshes/model.glb#Material7 (inverted)/nif"
        );
    }
}
