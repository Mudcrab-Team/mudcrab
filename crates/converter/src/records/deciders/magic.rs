//! Magic selector policies independently authored from native layout facts.
use super::{Context, FieldSchema, Selection};

/// Explicit registry used by schema assembly and runtime dispatch.
pub const NAMES: &[&str] = &["magic_associated_item"];

/// Select the associated slot using the enclosing MGEF DATA archetype.
pub fn select(
    name: &str,
    field: &FieldSchema,
    bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    if name != "magic_associated_item" {
        return Err(format!("unknown magic decider {name}"));
    }
    if context.record_type != *b"MGEF" || bytes.len() != 4 {
        return Err("associated magic slot requires MGEF and four bytes".into());
    }
    let parent = context
        .parent_payload
        .ok_or("associated magic slot lacks its parent DATA")?;
    let archetype = parent.get(64..68).ok_or("magic archetype is truncated")?;
    let archetype = u32::from_le_bytes(archetype.try_into().expect("checked archetype"));
    let selected = match archetype {
        12 => "light",
        17 => "bound_item",
        18 => "summoned_actor",
        25 | 40 => "hazard",
        34 => "peak_keyword",
        35 => "cloak_spell",
        36 | 46 => "race",
        39 => "weapon_enchantment",
        _ => "unused",
    };
    field
        .fields
        .iter()
        .position(|alternative| alternative.name == selected)
        .map(Selection::Alternative)
        .ok_or_else(|| format!("associated magic slot lacks {selected} alternative"))
}
