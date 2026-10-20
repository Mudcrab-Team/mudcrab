//! Independently authored selectors for native visual field variants.
use super::{Context, FieldSchema, Selection};

/// Explicit visual selector registry used by assembly and runtime dispatch.
pub const NAMES: &[&str] = &["visual_curve", "visual_legacy_image"];

/// Select only documented layouts and reject count disagreement within one field.
pub fn select(
    name: &str,
    field: &FieldSchema,
    bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    match name {
        "visual_legacy_image" => {
            if context.record_type != *b"IMGS" {
                return Err("legacy image selector requires IMGS".into());
            }
            let alternative = usize::from((15..=19).contains(&context.form_version));
            if alternative == 1 && bytes.len() < 40 {
                return Err("legacy image settings lack the ten-float prefix".into());
            }
            if field.fields.len() != 2 {
                return Err("legacy image schema lacks its two layouts".into());
            }
            Ok(Selection::Alternative(alternative))
        }
        "visual_curve" => {
            if context.record_type != *b"IMAD" || field.fields.len() != 1 {
                return Err("visual curve selector requires IMAD and one array layout".into());
            }
            let signature = field.signature.as_deref().ok_or("curve has no signature")?;
            let (offset, stride) = curve_count(signature.as_bytes())
                .ok_or_else(|| format!("undocumented visual curve {signature:?}"))?;
            let counts = context
                .preceding_subrecords
                .iter()
                .rev()
                .find(|(signature, _)| signature == b"DNAM")
                .map(|(_, payload)| *payload)
                .ok_or("visual curve lacks preceding DNAM counts")?;
            if counts.len() != 244 {
                return Err("visual curve DNAM counts are not244 bytes".into());
            }
            let count = u32::from_le_bytes(
                counts[offset..offset + 4]
                    .try_into()
                    .expect("bounded DNAM slot"),
            );
            let count = usize::try_from(count).map_err(|_| "visual curve count overflows")?;
            if count > 1_000_000 || count.checked_mul(stride) != Some(bytes.len()) {
                return Err(format!(
                    "visual curve count/extent mismatch: {count} keys of{stride} bytes, payload{}",
                    bytes.len()
                ));
            }
            Ok(Selection::Alternative(0))
        }
        _ => Err(format!("unknown visual selector {name}")),
    }
}

/// Map native curve signatures to independently observed DNAM count slots.
fn curve_count(signature: &[u8]) -> Option<(usize, usize)> {
    if signature.len() == 4 && &signature[1..] == b"IAD" {
        let index = signature[0];
        if index <= 0x14 {
            return Some((8 + usize::from(index) * 8, 8));
        }
        if (0x40..=0x54).contains(&index) {
            return Some((12 + usize::from(index - 0x40) * 8, 8));
        }
    }
    Some(match signature {
        b"TNAM" => (176, 20),
        b"BNAM" => (180, 8),
        b"VNAM" => (184, 8),
        b"RNAM" => (188, 8),
        b"SNAM" => (192, 8),
        b"UNAM" => (196, 8),
        b"WNAM" => (212, 8),
        b"XNAM" => (216, 8),
        b"YNAM" => (220, 8),
        b"NAM1" => (228, 8),
        b"NAM2" => (232, 8),
        b"NAM3" => (236, 20),
        b"NAM4" => (240, 8),
        _ => return None,
    })
}
