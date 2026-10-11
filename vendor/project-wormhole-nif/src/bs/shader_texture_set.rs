use project_wormhole_ba2::dev::{ensure_texture_parent, normalize_esm_path};

use super::prelude::*;

#[derive(Debug, Clone)]
pub struct BSShaderTextureSet {
    /// Raw semantic slots in serialized order. The shader property, rather
    /// than the file name, determines how Skyrim interprets each slot.
    pub textures: Vec<Option<String>>,
    pub diffuse: Option<String>,
    pub normal: Option<String>,
    pub glow: Option<String>,
    pub height_or_detail: Option<String>,
    pub environment: Option<String>,
    pub environment_mask: Option<String>,
    pub inner_layer: Option<String>,
    pub specular: Option<String>,
}

impl Parse<&[u8]> for BSShaderTextureSet {
    fn parse(i: &[u8]) -> IResult<&[u8], Self> {
        let (i, string_count) = le_u32(i)?;
        let (i, textures) = count(SizedString32::parse, string_count as usize)(i)?;

        let fixed: Vec<Option<String>> = textures
            .into_iter()
            .map(|texture| {
                // Official Skyrim assets use these four-byte internal
                // sentinels for an intentionally absent/error texture. They
                // are not filesystem paths and must never become glTF URIs.
                if matches!(
                    texture.0.as_bytes(),
                    [0x08, b'N', b'O', b'R'] | [0x08, b'E', b'R', b'R']
                ) {
                    return None;
                }
                sanitize_texture_slot(&texture.0)
            })
            .collect();

        // BSShaderTextureSet has semantic slots; diffuse names commonly end
        // in "rocks01.dds" rather than "_d.dds", so suffix classification
        // silently discarded most official Skyrim base-color textures.
        let tset = BSShaderTextureSet {
            textures: fixed.clone(),
            diffuse: fixed.first().cloned().flatten(),
            normal: fixed.get(1).cloned().flatten(),
            glow: fixed.get(2).cloned().flatten(),
            height_or_detail: fixed.get(3).cloned().flatten(),
            environment: fixed.get(4).cloned().flatten(),
            environment_mask: fixed.get(5).cloned().flatten(),
            inner_layer: fixed.get(6).cloned().flatten(),
            specular: fixed.get(7).cloned().flatten(),
        };

        Ok((i, tset))
    }
}

/// Repairs NIF-embedded authoring paths before canonicalization. Slots with
/// control characters are corrupt padding rather than paths and map to an
/// absent texture, like the engine sentinels above. A leading Windows drive
/// prefix marks an absolute workspace leak; stripping it leaves a relative
/// path that the downstream canonicalizer still validates strictly (its
/// last-folder repair recovers the shipped `textures/...` suffix).
fn sanitize_texture_slot(raw: &str) -> Option<String> {
    let mut fixed_path = normalize_esm_path(raw);
    if fixed_path.chars().any(char::is_control) {
        return None;
    }
    fixed_path = fixed_path.trim().to_owned();
    let bytes = fixed_path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        fixed_path = fixed_path[2..].trim_start_matches('/').to_owned();
    }
    ensure_texture_parent(&mut fixed_path);
    (fixed_path != "textures/").then_some(fixed_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_skyrim_texture_slots_without_filename_suffix_guessing() {
        let values = [
            "landscape/rocks01.dds",
            "landscape/rocks01_n.dds",
            "landscape/rocks01_g.dds",
            "",
            "",
            "",
            "",
            "landscape/rocks01_s.dds",
            "",
        ];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
        for value in values {
            bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        let (_, set) = BSShaderTextureSet::parse(&bytes).unwrap();
        assert_eq!(
            set.diffuse.as_deref(),
            Some("textures/landscape/rocks01.dds")
        );
        assert_eq!(
            set.normal.as_deref(),
            Some("textures/landscape/rocks01_n.dds")
        );
        assert_eq!(
            set.glow.as_deref(),
            Some("textures/landscape/rocks01_g.dds")
        );
        assert_eq!(
            set.specular.as_deref(),
            Some("textures/landscape/rocks01_s.dds")
        );
        assert_eq!(set.textures.len(), values.len());
    }

    #[test]
    fn strips_authoring_drive_prefix_from_absolute_texture_paths() {
        assert_eq!(
            sanitize_texture_slot(
                r"c:\program files (x86)\steam\steamapps\common\cc-s\data\textures\creationclub\cbhsse001\glass\gaunts2.dds"
            )
                .as_deref(),
            Some(
                "textures/program files (x86)/steam/steamapps/common/cc-s/data/textures/creationclub/cbhsse001/glass/gaunts2.dds"
            )
        );
        assert_eq!(sanitize_texture_slot("c:"), None);
        assert_eq!(sanitize_texture_slot("c:/"), None);
    }

    #[test]
    fn drops_slots_with_control_characters_as_absent() {
        assert_eq!(sanitize_texture_slot("\n"), None);
        assert_eq!(sanitize_texture_slot("textures\\\ntextures\\"), None);
        assert_eq!(
            sanitize_texture_slot("textures/armor/glass/m/gauntlet.dds").as_deref(),
            Some("textures/armor/glass/m/gauntlet.dds")
        );
    }

    #[test]
    fn omits_whitespace_padded_empty_texture_slots() {
        for placeholder in [" ", "   ", "textures/ ", "textures\\ "] {
            let values = [
                "textures/architecture/farmhouse/stonewall01.dds",
                "textures/architecture/farmhouse/stonewall01_n.dds",
                "",
                placeholder,
            ];
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
                bytes.extend_from_slice(value.as_bytes());
            }
            let (_, set) = BSShaderTextureSet::parse(&bytes).unwrap();
            assert_eq!(set.diffuse.as_deref(), Some(values[0]));
            assert_eq!(set.normal.as_deref(), Some(values[1]));
            assert_eq!(set.textures[3], None, "placeholder: {placeholder:?}");
            assert_eq!(set.height_or_detail, None);
        }
    }

    #[test]
    fn omits_skyrim_internal_texture_sentinels() {
        let values: [&[u8]; 3] = [b"textures/a.dds", b"\x08NOR", b"\x08ERR"];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
        for value in values {
            bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
            bytes.extend_from_slice(value);
        }
        let (_, set) = BSShaderTextureSet::parse(&bytes).unwrap();
        assert_eq!(set.diffuse.as_deref(), Some("textures/a.dds"));
        assert_eq!(set.textures[1], None);
        assert_eq!(set.textures[2], None);
    }
}
