//! Named compatibility policies for the current u32 publication formats.
//! Native FormKey ownership is resolved first and always uses the common self rule.

/// Preserve the old NAVM ONAM NULL projection pending separate schema-layout work.
/// This policy is source-context based, never a plugin/record allowlist.
/// Only NAVM ONAM uses this adapter: official words look float-like, but their
/// layout is unproven. Other optional links resolve self before ordinary target validation.
pub(crate) struct LegacyTypedReaderProjection;

impl LegacyTypedReaderProjection {
    /// Whether the scoped NAVM ONAM publication keeps this resolved self link NULL.
    pub(crate) fn maps_to_null(
        raw: u32,
        master_count: usize,
        optional: bool,
        record_type: Option<[u8; 4]>,
        field: Option<&str>,
    ) -> bool {
        optional
            && record_type == Some(*b"NAVM")
            && field.is_some_and(|field| field.starts_with("ONAM[source:"))
            && (raw >> 24) as usize > master_count
    }
}
