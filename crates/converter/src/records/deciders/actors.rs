//! Actor selectors, owned by phase-2 agent B.
use super::{Context, FieldSchema, Selection};
/// Names implemented by this module, checked during schema assembly.
pub const NAMES: &[&str] = &[];
/// Select an authored actor-family union alternative.
pub fn select(
    name: &str,
    _field: &FieldSchema,
    _bytes: &[u8],
    _context: &Context<'_>,
) -> Result<Selection, String> {
    Err(format!("unknown actor decider {name}"))
}
