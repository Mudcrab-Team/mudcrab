//! Magic and effects selectors, owned by phase-2 agent C.
use super::{Context, FieldSchema, Selection};
/// Names implemented by this module, checked during schema assembly.
pub const NAMES: &[&str] = &[];
/// Select an authored magic-family union alternative.
pub fn select(
    name: &str,
    _field: &FieldSchema,
    _bytes: &[u8],
    _context: &Context<'_>,
) -> Result<Selection, String> {
    Err(format!("unknown magic decider {name}"))
}
