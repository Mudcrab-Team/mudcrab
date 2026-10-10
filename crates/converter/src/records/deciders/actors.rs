//! Independently authored actor layout selectors.
//!
//! Format facts: NPC ACBS bit0x80 selects a signed level multiplier encoded in
//! thousandths; FACT PLVD has a signed location discriminator preceding its
//! same-width value. Both policies leave primitive bytes unchanged. The shared
//! decoder catches errors as bounded field/candidate diagnostics.
use super::{Context, FieldSchema, Selection};

/// Names implemented by this module, checked during schema assembly.
pub const NAMES: &[&str] = &["actors_npc_configuration", "actors_vendor_location"];

/// Select an authored actor-family union alternative.
pub fn select(
    name: &str,
    field: &FieldSchema,
    bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    let index = match name {
        "actors_npc_configuration" => {
            if context.record_type != *b"NPC_" || bytes.len() != 24 {
                return Err("NPC configuration requires a 24-byte ACBS payload".into());
            }
            let flags = u32::from_le_bytes(bytes[..4].try_into().expect("checked ACBS width"));
            usize::from(flags & 0x80 != 0)
        }
        "actors_vendor_location" => {
            if context.record_type != *b"FACT" || bytes.len() != 12 {
                return Err("vendor location requires a 12-byte FACT PLVD payload".into());
            }
            let discriminator =
                i32::from_le_bytes(bytes[..4].try_into().expect("checked PLVD width"));
            if !(0..=12).contains(&discriminator) {
                return Err(format!("unknown vendor location kind {discriminator}"));
            }
            discriminator as usize
        }
        _ => return Err(format!("unknown actor decider {name}")),
    };
    if index >= field.fields.len() {
        return Err(format!("actor schema lacks {name} alternative {index}"));
    }
    Ok(Selection::Alternative(index))
}
