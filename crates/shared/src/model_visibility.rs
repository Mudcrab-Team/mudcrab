//! Converted primitive exclusions and conservative proofs of MASK shader discard.

use serde_json::Value;

/// Match the primitive declaration used by runtime readiness and collision inspection.
pub fn explicit_material_exclusion(extras: &Value) -> bool {
    extras
        .pointer("/openSkyrim/materialExclusion")
        .is_some_and(Value::is_string)
}

/// The converter declares excluded NIF shapes with a non-rendering fallback material.
pub fn declared_non_rendering(material: &Value) -> bool {
    material
        .pointer("/extras/openSkyrim/nonRenderingExclusion")
        .and_then(Value::as_bool)
        == Some(true)
}

/// Mirror the runtime's strict MASK discard: equality passes, OPAQUE ignores alpha.
/// Textures have alpha at most one. Without vertex colors or alpha-replacing shader
/// extensions, the material factor is therefore an upper bound on fragment alpha.
pub fn mask_cannot_render(material: &Value, has_vertex_colors: bool) -> bool {
    if material.get("alphaMode").and_then(Value::as_str) != Some("MASK") || has_vertex_colors {
        return false;
    }
    if let Some(extensions) = material.get("extensions") {
        let Some(extensions) = extensions.as_object() else {
            return false;
        };
        for (name, value) in extensions {
            match name.as_str() {
                "KHR_materials_emissive_strength"
                | "KHR_materials_specular"
                | "KHR_materials_unlit" => {}
                "OPEN_SKYRIM_material" => {
                    // Greyscale_To_PaletteAlpha replaces alpha rather than multiplying it.
                    if value.get("shaderFamily").and_then(Value::as_str) == Some("effect")
                        && value
                            .get("shaderFlags1")
                            .and_then(Value::as_u64)
                            .is_none_or(|flags| flags & (1 << 5) != 0)
                    {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }
    let cutoff = match material.get("alphaCutoff") {
        Some(value) => value.as_f64().map(|value| value as f32),
        None => Some(0.5),
    };
    let alpha = match material.pointer("/pbrMetallicRoughness/baseColorFactor") {
        Some(value) => value
            .as_array()
            .filter(|v| v.len() == 4)
            .and_then(|v| v[3].as_f64())
            .map(|value| value as f32),
        None => Some(1.0),
    };
    matches!((alpha, cutoff), (Some(alpha), Some(cutoff))
        if (0.0..=1.0).contains(&alpha) && (0.0..=1.0).contains(&cutoff) && alpha < cutoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mask_proof_keeps_equality_opaque_colors_and_alpha_replacement() {
        let mut material =
            json!({"alphaMode":"MASK","pbrMetallicRoughness":{"baseColorFactor":[1,1,1,0]}});
        assert!(mask_cannot_render(&material, false));
        assert!(!mask_cannot_render(&material, true));
        material["alphaCutoff"] = json!(0);
        assert!(!mask_cannot_render(&material, false));
        material["alphaCutoff"] = json!(0.5);
        material["pbrMetallicRoughness"]["baseColorFactor"][3] = json!(0.5);
        assert!(!mask_cannot_render(&material, false));
        material["pbrMetallicRoughness"]["baseColorFactor"][3] = json!(0.49);
        assert!(mask_cannot_render(&material, false));
        material["pbrMetallicRoughness"]["baseColorFactor"][3] = json!(0.499999999);
        assert!(
            !mask_cannot_render(&material, false),
            "the runtime rounds both inputs to f32"
        );
        material["alphaMode"] = json!("OPAQUE");
        assert!(!mask_cannot_render(&material, false));
        material["alphaMode"] = json!("MASK");
        material["extensions"] =
            json!({"OPEN_SKYRIM_material":{"shaderFamily":"effect","shaderFlags1":32}});
        assert!(!mask_cannot_render(&material, false));
        material["extensions"] = json!({"unknown_alpha_extension":{}});
        assert!(!mask_cannot_render(&material, false));
    }
}
