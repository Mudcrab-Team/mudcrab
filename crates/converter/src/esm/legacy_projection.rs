//! Named compatibility policies for the current u32 publication formats.
//! Native FormKey ownership is resolved first and always uses the common self rule.

/// Preserve the schema reader's old optional-link NULL projection until its writer flag day.
/// This policy is source-context based, never a plugin/record allowlist.
/// It applies only to the typed reader's existing u32 publication path, after native
/// resolution. In particular, NAVM ONAM can contain float-looking words currently
/// described as FormIDs (suspected schema mis-typing); changing that schema is
/// separate work. Native FormKeys and the handwritten reader keep the self rule.
pub(crate) struct LegacyTypedReaderProjection;

impl LegacyTypedReaderProjection {
    /// Whether the old typed u32 projection cleared this already-resolved native link.
    pub(crate) fn maps_to_null(raw: u32, master_count: usize, optional: bool) -> bool {
        optional && (raw >> 24) as usize > master_count
    }
}
