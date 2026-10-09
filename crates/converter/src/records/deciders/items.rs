//! Independently authored item and crafting layout selectors.
use super::{Context, FieldSchema, Selection};

/// Names owned by this family and accepted during authored-schema validation.
pub const NAMES: &[&str] = &["book_teaching"];

/// Select the BOOK teaching member using its enclosing DATA flags.
/// The spell mask is 0x04; skill indices remain signed and never become links.
pub fn select(
    name: &str,
    field: &FieldSchema,
    bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    if name != "book_teaching" {
        return Err(format!("unknown item decider {name}"));
    }
    if context.record_type != *b"BOOK" || bytes.len() != 4 {
        return Err("BOOK teaching needs a four-byte member in BOOK".into());
    }
    let parent = context
        .parent_payload
        .ok_or("BOOK teaching needs DATA context")?;
    let flags = *parent.first().ok_or("BOOK teaching flags are truncated")?;
    let kind = if flags & 0x04 != 0 { "form_id" } else { "i32" };
    let alternative = field
        .fields
        .iter()
        .position(|candidate| candidate.kind == kind)
        .ok_or_else(|| format!("BOOK teaching lacks {kind} alternative"))?;
    Ok(Selection::Alternative(alternative))
}
